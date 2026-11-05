//! Annotated Page Content (APC) snapshot source. Experimental.
//!
//! `Page.getAnnotatedPageContent` returns a base64 protobuf (`AnnotatedPageContent`) describing
//! the rendered page: a sparse content tree with geometry, interaction info and DOM node ids.
//! This module decodes the payload with a small wire-format reader, then renders a compact text
//! tree whose actionable nodes map onto the `@eN` ref system through their backend node ids.
//!
//! Chrome verification: payloads from Chrome 157 (stable). See `VERIFIED_CHROME_MAJORS`.
//!
//! Schema: `components/optimization_guide/proto/features/common_quality_data.proto`, Chromium
//! main at commit `0379cdad118764a1901083a0fc1e77ec3145d4be` (2026-11-04), verified against
//! payloads from Chrome 157. The schema is internal and may change without notice, so only the
//! fields listed below are read and every other field is skipped. Any decode failure is reported
//! to the caller, which falls back to the accessibility-tree snapshot.
//!
//! Field numbers read (message.field):
//! - `AnnotatedPageContent`: 2 root_node, 3 main_frame_data, 5 viewport_geometry
//! - `ContentNode`: 1 children_nodes, 2 content_attributes
//! - `ContentAttributes`: 2 common_ancestor_dom_node_id, 3 attribute_type, 4 geometry,
//!   9 iframe_data, 10 text_data, 11 image_data, 14 anchor_data, 16 label, 19 form_data,
//!   20 form_control_data, 21 interaction_info, 25 label_for_dom_node_id, 26 aria_role
//! - `Geometry`: 1 outer_bounding_box, 2 visible_bounding_box
//! - `InteractionInfo`: 7 is_focusable, 15 clickability_reasons, 16 is_disabled,
//!   17 interaction_disabled_reasons, 18 is_tabbable
//! - `FormControlData`: 1 field_name, 2 field_value, 3 form_control_type, 4 select_options,
//!   5 placeholder, 6 is_checked, 7 is_required, 11 is_readonly
//! - `IframeData`: 3 frame_data, 4 redacted_frame_metadata
//! - `FrameData`: 1 security_origin, 4 url, 5 title

use std::cell::Cell;
use std::collections::HashMap;

/// Where the snapshot tree comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SnapshotSource {
    /// Accessibility tree (default).
    #[default]
    Ax,
    /// Annotated Page Content only.
    Apc,
}

impl SnapshotSource {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "ax" => Some(Self::Ax),
            "apc" => Some(Self::Apc),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ax => "ax",
            Self::Apc => "apc",
        }
    }
}

/// Nesting limit for the recursive decoder. Protects the daemon stack from hostile pages.
const MAX_DECODE_DEPTH: usize = 1000;
/// Longest accessible name for a clickable container with no label, matching the AX path's
/// cursor-interactive text limit.
const CLICKABLE_NAME_CHARS: usize = 100;

// ContentAttributeType values.
const KIND_ROOT: u32 = 1;
const KIND_ANCHOR: u32 = 22;
const KIND_CANVAS: u32 = 26;
const KIND_DIALOG_MODAL: u32 = 28;
const KIND_DIALOG_MODELESS: u32 = 29;
const KIND_FORM: u32 = 8;
const KIND_FORM_CONTROL: u32 = 24;
const KIND_HEADING: u32 = 5;
const KIND_IFRAME: u32 = 3;
const KIND_IMAGE: u32 = 9;
const KIND_LIST_ITEM: u32 = 23;
const KIND_ORDERED_LIST: u32 = 6;
const KIND_PARAGRAPH: u32 = 4;
const KIND_TABLE: u32 = 10;
const KIND_TABLE_CELL: u32 = 11;
const KIND_TABLE_ROW: u32 = 21;
const KIND_TEXT: u32 = 20;
const KIND_UNORDERED_LIST: u32 = 7;

// ClickabilityReason values that do not by themselves make a node a click target.
const REASON_MOUSE_HOVER: u32 = 13;
const REASON_HOVER_PSEUDO_CLASS: u32 = 14;
const REASON_MOUSE_OVER: u32 = 18;
const REASON_ARIA_EXPANDED_TRUE: u32 = 8;
const REASON_ARIA_EXPANDED_FALSE: u32 = 9;

// InteractionDisabledReason values.
const DISABLED_POINTER_EVENTS_NONE: u32 = 6;
const DISABLED_VISUALLY_UNREACHABLE: u32 = 7;

// FormControlType values.
const CONTROL_CONTENT_EDITABLE: u32 = 1;
const CONTROL_CHECKBOX: u32 = 2;
const CONTROL_NUMBER: u32 = 5;
const CONTROL_RADIO: u32 = 7;
const CONTROL_SEARCH: u32 = 8;
const CONTROL_SELECT_ONE: u32 = 12;
const CONTROL_SELECT_MULTIPLE: u32 = 13;
const CONTROL_BUTTON_BUTTON: u32 = 16;
const CONTROL_BUTTON_SUBMIT: u32 = 17;
const CONTROL_BUTTON_RESET: u32 = 18;
const CONTROL_BUTTON_POPOVER: u32 = 19;
const CONTROL_FIELDSET: u32 = 20;
const CONTROL_INPUT_BUTTON: u32 = 21;
const CONTROL_INPUT_FILE: u32 = 25;
const CONTROL_INPUT_HIDDEN: u32 = 26;
const CONTROL_INPUT_IMAGE: u32 = 27;
const CONTROL_RANGE: u32 = 28;
const CONTROL_INPUT_RESET: u32 = 29;
const CONTROL_INPUT_SUBMIT: u32 = 30;
const CONTROL_OUTPUT: u32 = 33;

// ---------------------------------------------------------------------------
// Model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    pub fn is_empty(&self) -> bool {
        self.width <= 0 || self.height <= 0
    }
}

#[derive(Debug, Clone, Default)]
pub struct SelectOption {
    pub value: String,
    pub text: String,
    pub selected: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Control {
    pub name: String,
    pub value: String,
    pub kind: u32,
    pub options: Vec<SelectOption>,
    pub placeholder: String,
    pub checked: bool,
    pub required: bool,
    pub readonly: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Interaction {
    pub focusable: bool,
    pub tabbable: bool,
    pub disabled: bool,
    pub reasons: Vec<u32>,
    pub disabled_reasons: Vec<u32>,
}

#[derive(Debug, Clone, Default)]
pub struct Frame {
    pub url: String,
    pub origin: Option<String>,
    pub opaque: bool,
    /// Set for frames whose content Chrome withheld (`RedactedFrameMetadata`).
    pub redacted: bool,
    /// True when the frame shares the main frame's origin all the way down, so its DOM ids
    /// belong to the page session.
    pub same_origin: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Node {
    pub kind: u32,
    pub dom_id: Option<i64>,
    pub outer: Option<Rect>,
    pub visible: Option<Rect>,
    pub text: Option<String>,
    pub caption: Option<String>,
    pub url: Option<String>,
    pub form_name: Option<String>,
    pub label: Option<String>,
    pub label_for: Option<i64>,
    pub aria_role: u32,
    pub control: Option<Control>,
    pub frame: Option<Frame>,
    pub info: Option<Interaction>,
    pub children: Vec<Node>,
    /// Frame scope: 0 is the main frame, each iframe's content gets its own id. DOM ids are only
    /// unique within a renderer process, so lookups key on (scope, dom id).
    pub scope: u32,
    /// True when the node's ids cannot be used from the page session (cross-origin or opaque
    /// iframe content). Such nodes render read-only.
    pub read_only: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Doc {
    pub root: Node,
    pub main_origin: Option<String>,
    pub viewport: Option<Rect>,
    /// Fields in the payload that the decoder does not read (unknown to it, or known with a
    /// different wire type). Expected to be non-zero; reported in the debug log.
    pub unread_fields: usize,
}

thread_local! {
    static UNREAD: Cell<usize> = const { Cell::new(0) };
}

fn note_unread() {
    UNREAD.with(|c| c.set(c.get() + 1));
}

// ---------------------------------------------------------------------------
// Wire reader
// ---------------------------------------------------------------------------

enum Wire<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
    Fixed,
}

struct Fields<'a> {
    buf: &'a [u8],
    pos: usize,
}

fn read_varint(buf: &[u8], pos: &mut usize) -> Result<u64, String> {
    let mut value = 0u64;
    let mut shift = 0u32;
    loop {
        let byte = *buf.get(*pos).ok_or("truncated varint")?;
        *pos += 1;
        if shift >= 64 {
            return Err("varint overflow".into());
        }
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
        shift += 7;
    }
}

impl<'a> Iterator for Fields<'a> {
    type Item = Result<(u32, Wire<'a>), String>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.buf.len() {
            return None;
        }
        Some(self.read())
    }
}

impl<'a> Fields<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn read(&mut self) -> Result<(u32, Wire<'a>), String> {
        let tag = read_varint(self.buf, &mut self.pos)?;
        let field = u32::try_from(tag >> 3).map_err(|_| "field number overflow".to_string())?;
        match tag & 7 {
            0 => Ok((field, Wire::Varint(read_varint(self.buf, &mut self.pos)?))),
            1 => self.skip(field, 8),
            2 => {
                let len = usize::try_from(read_varint(self.buf, &mut self.pos)?)
                    .map_err(|_| "length overflow".to_string())?;
                let end = self.pos.checked_add(len).ok_or("length overflow")?;
                let bytes = self.buf.get(self.pos..end).ok_or("truncated field")?;
                self.pos = end;
                Ok((field, Wire::Bytes(bytes)))
            }
            5 => self.skip(field, 4),
            other => Err(format!("unsupported wire type {other}")),
        }
    }

    fn skip(&mut self, field: u32, len: usize) -> Result<(u32, Wire<'a>), String> {
        let end = self.pos.checked_add(len).ok_or("length overflow")?;
        if end > self.buf.len() {
            return Err("truncated fixed field".into());
        }
        self.pos = end;
        Ok((field, Wire::Fixed))
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn push_ints(wire: &Wire, out: &mut Vec<u32>) -> Result<(), String> {
    match wire {
        Wire::Varint(v) => out.push(*v as u32),
        Wire::Bytes(bytes) => {
            let mut pos = 0;
            while pos < bytes.len() {
                out.push(read_varint(bytes, &mut pos)? as u32);
            }
        }
        Wire::Fixed => {}
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Decoder
// ---------------------------------------------------------------------------

fn decode_rect(buf: &[u8]) -> Result<Rect, String> {
    let mut rect = Rect::default();
    for field in Fields::new(buf) {
        if let (field, Wire::Varint(v)) = field? {
            match field {
                1 => rect.x = v as i32,
                2 => rect.y = v as i32,
                3 => rect.width = v as i32,
                4 => rect.height = v as i32,
                _ => {}
            }
        }
    }
    Ok(rect)
}

fn decode_geometry(buf: &[u8], node: &mut Node) -> Result<(), String> {
    for field in Fields::new(buf) {
        if let (field, Wire::Bytes(b)) = field? {
            match field {
                1 => node.outer = Some(decode_rect(b)?),
                2 => node.visible = Some(decode_rect(b)?),
                _ => {}
            }
        }
    }
    Ok(())
}

fn decode_interaction(buf: &[u8]) -> Result<Interaction, String> {
    let mut info = Interaction::default();
    for field in Fields::new(buf) {
        let (field, wire) = field?;
        match field {
            7 => info.focusable = matches!(wire, Wire::Varint(v) if v != 0),
            15 => push_ints(&wire, &mut info.reasons)?,
            16 => info.disabled = matches!(wire, Wire::Varint(v) if v != 0),
            17 => push_ints(&wire, &mut info.disabled_reasons)?,
            18 => info.tabbable = matches!(wire, Wire::Varint(v) if v != 0),
            _ => note_unread(),
        }
    }
    Ok(info)
}

fn decode_option(buf: &[u8]) -> Result<SelectOption, String> {
    let mut option = SelectOption::default();
    for field in Fields::new(buf) {
        match field? {
            (1, Wire::Bytes(b)) => option.value = text(b),
            (2, Wire::Bytes(b)) => option.text = text(b),
            (3, Wire::Varint(v)) => option.selected = v != 0,
            _ => {}
        }
    }
    Ok(option)
}

fn decode_control(buf: &[u8]) -> Result<Control, String> {
    let mut control = Control::default();
    for field in Fields::new(buf) {
        match field? {
            (1, Wire::Bytes(b)) => control.name = text(b),
            (2, Wire::Bytes(b)) => control.value = text(b),
            (3, Wire::Varint(v)) => control.kind = v as u32,
            (4, Wire::Bytes(b)) => control.options.push(decode_option(b)?),
            (5, Wire::Bytes(b)) => control.placeholder = text(b),
            (6, Wire::Varint(v)) => control.checked = v != 0,
            (7, Wire::Varint(v)) => control.required = v != 0,
            (11, Wire::Varint(v)) => control.readonly = v != 0,
            _ => {}
        }
    }
    Ok(control)
}

fn decode_origin(buf: &[u8]) -> Result<(bool, String), String> {
    let mut opaque = false;
    let mut value = String::new();
    for field in Fields::new(buf) {
        match field? {
            (1, Wire::Varint(v)) => opaque = v != 0,
            (2, Wire::Bytes(b)) => value = text(b),
            _ => {}
        }
    }
    Ok((opaque, value))
}

fn decode_frame_data(buf: &[u8]) -> Result<Frame, String> {
    let mut frame = Frame::default();
    for field in Fields::new(buf) {
        match field? {
            (1, Wire::Bytes(b)) => {
                let (opaque, value) = decode_origin(b)?;
                frame.opaque = opaque;
                if !value.is_empty() {
                    frame.origin = Some(value);
                }
            }
            (4, Wire::Bytes(b)) => frame.url = text(b),
            _ => {}
        }
    }
    Ok(frame)
}

fn decode_iframe(buf: &[u8]) -> Result<Frame, String> {
    let mut frame = Frame {
        redacted: true,
        ..Frame::default()
    };
    for field in Fields::new(buf) {
        if let (3, Wire::Bytes(b)) = field? {
            frame = decode_frame_data(b)?;
        }
    }
    Ok(frame)
}

fn first_string(buf: &[u8], wanted: u32) -> Result<Option<String>, String> {
    for field in Fields::new(buf) {
        if let (field, Wire::Bytes(b)) = field? {
            if field == wanted {
                return Ok(Some(text(b)));
            }
        }
    }
    Ok(None)
}

fn decode_attributes(buf: &[u8], node: &mut Node) -> Result<(), String> {
    for field in Fields::new(buf) {
        let (field, wire) = field?;
        match (field, wire) {
            (2, Wire::Varint(v)) => node.dom_id = Some(i64::from(v as i32)),
            (3, Wire::Varint(v)) => node.kind = v as u32,
            (4, Wire::Bytes(b)) => decode_geometry(b, node)?,
            (9, Wire::Bytes(b)) => node.frame = Some(decode_iframe(b)?),
            (10, Wire::Bytes(b)) => node.text = first_string(b, 1)?,
            (11, Wire::Bytes(b)) => node.caption = first_string(b, 2)?,
            (14, Wire::Bytes(b)) => node.url = first_string(b, 1)?,
            (16, Wire::Bytes(b)) => node.label = Some(text(b)),
            (19, Wire::Bytes(b)) => node.form_name = first_string(b, 1)?,
            (20, Wire::Bytes(b)) => node.control = Some(decode_control(b)?),
            (21, Wire::Bytes(b)) => node.info = Some(decode_interaction(b)?),
            (25, Wire::Varint(v)) => node.label_for = Some(i64::from(v as i32)),
            (26, Wire::Varint(v)) => node.aria_role = v as u32,
            _ => note_unread(),
        }
    }
    Ok(())
}

fn decode_node(buf: &[u8], depth: usize) -> Result<Node, String> {
    if depth > MAX_DECODE_DEPTH {
        return Err("content tree is nested too deeply".into());
    }
    let mut node = Node::default();
    for field in Fields::new(buf) {
        match field? {
            (1, Wire::Bytes(b)) => node.children.push(decode_node(b, depth + 1)?),
            (2, Wire::Bytes(b)) => decode_attributes(b, &mut node)?,
            _ => note_unread(),
        }
    }
    Ok(node)
}

/// Assign frame scopes and read-only flags. Frames are same-process only when their origin is a
/// non-opaque match for the main frame's origin; anything else (including redacted frames and
/// everything nested below them) is read-only.
fn annotate(
    node: &mut Node,
    scope: u32,
    next_scope: &mut u32,
    read_only: bool,
    main: Option<&str>,
) {
    node.scope = scope;
    node.read_only = read_only;
    let (child_scope, child_read_only) = match &mut node.frame {
        Some(frame) => {
            *next_scope += 1;
            frame.same_origin = !read_only
                && !frame.redacted
                && !frame.opaque
                && frame.origin.is_some()
                && frame.origin.as_deref() == main;
            (*next_scope, !frame.same_origin)
        }
        None => (scope, read_only),
    };
    for child in &mut node.children {
        annotate(child, child_scope, next_scope, child_read_only, main);
    }
}

/// Decode a `Page.getAnnotatedPageContent` payload (raw protobuf bytes).
pub fn decode(bytes: &[u8]) -> Result<Doc, String> {
    UNREAD.with(|c| c.set(0));
    let mut doc = Doc::default();
    let mut has_root = false;
    for field in Fields::new(bytes) {
        match field? {
            (2, Wire::Bytes(b)) => {
                doc.root = decode_node(b, 0)?;
                has_root = true;
            }
            (3, Wire::Bytes(b)) => {
                doc.main_origin = decode_frame_data(b)?.origin;
            }
            (5, Wire::Bytes(b)) => doc.viewport = Some(decode_rect(b)?),
            _ => note_unread(),
        }
    }
    if !has_root {
        return Err("payload has no root node".into());
    }
    let main = doc.main_origin.clone();
    let mut next_scope = 0;
    annotate(&mut doc.root, 0, &mut next_scope, false, main.as_deref());
    doc.unread_fields = UNREAD.with(Cell::get);
    Ok(doc)
}

// ---------------------------------------------------------------------------
// Drift checks
// ---------------------------------------------------------------------------

/// Chrome major versions whose payloads the decoder was verified against. Outside this range apc
/// is still attempted, with stricter drift checks. To extend it, re-run the decoder against
/// payloads recorded from the new major (the fixtures in `test_fixtures` and the e2e snapshot
/// tests), confirm nothing in the field list above moved, then widen the range.
pub const VERIFIED_CHROME_MAJORS: std::ops::RangeInclusive<u32> = 157..=157;

/// Node ids probed with `DOM.describeNode` per snapshot.
const SAMPLE_LIMIT: usize = 16;
/// Probe limit when the Chrome version is outside the verified range.
const STRICT_SAMPLE_LIMIT: usize = 64;
/// Unresolved share of the sample that triggers the fallback with a verified Chrome version.
/// Outside the verified range any unresolved id does.
const MAX_UNRESOLVED_PERCENT: usize = 25;

/// DOM elements an agent can plausibly act on. A page that has some while apc reports none is a
/// sign the payload layout changed.
pub const INTERACTIVE_SELECTOR: &str =
    "a, button, input, select, textarea, [role], [onclick], [tabindex]";

/// Major version from a `Browser.getVersion` product string such as `Chrome/157.0.8103.39`.
pub fn chrome_major(product: &str) -> Option<u32> {
    let version = product.rsplit('/').next()?;
    version.split('.').next()?.parse().ok()
}

/// True when the decoder was verified against this Chrome version. Unknown versions are not.
pub fn is_verified_version(product: &str) -> bool {
    chrome_major(product).is_some_and(|major| VERIFIED_CHROME_MAJORS.contains(&major))
}

/// One-line warning for a snapshot that fell back to the accessibility tree.
pub fn fallback_warning(source: SnapshotSource, chrome: &str, reason: &str) -> String {
    format!(
        "# warning: --source {} unavailable ({chrome}): {reason}; using the ax snapshot",
        source.as_str()
    )
}

/// What the page says about itself, gathered over CDP after the payload decoded.
#[derive(Debug, Clone, Copy, Default)]
pub struct PageFacts {
    /// Elements in the DOM matching `INTERACTIVE_SELECTOR`.
    pub dom_interactive: usize,
    /// Sampled node ids that did not resolve.
    pub unresolved: usize,
}

/// A decoded payload that passed the checks that need no browser.
pub struct Vetted {
    doc: Doc,
    actionable: usize,
    sample: Vec<i64>,
    strict: bool,
}

fn count_actionable(node: &Node) -> usize {
    usize::from(is_actionable(node)) + node.children.iter().map(count_actionable).sum::<usize>()
}

fn collect_ids(node: &Node, out: &mut Vec<i64>) {
    if !node.read_only && is_actionable(node) {
        if let Some(id) = node.dom_id {
            if !out.contains(&id) {
                out.push(id);
            }
        }
    }
    for child in &node.children {
        collect_ids(child, out);
    }
}

fn check_structure(doc: &Doc) -> Result<(), String> {
    if doc.root.kind != KIND_ROOT {
        return Err(format!(
            "unexpected root node type {} (expected {KIND_ROOT})",
            doc.root.kind
        ));
    }
    if !doc.viewport.is_some_and(|v| !v.is_empty()) {
        return Err("payload has no viewport geometry".into());
    }
    Ok(())
}

impl Vetted {
    /// Decode and check the structure. `strict` is set when the Chrome version is unverified.
    pub fn new(bytes: &[u8], strict: bool) -> Result<Self, String> {
        let doc =
            decode(bytes).map_err(|e| format!("could not decode annotated page content: {e}"))?;
        check_structure(&doc)?;
        let mut ids = Vec::new();
        collect_ids(&doc.root, &mut ids);
        let limit = if strict {
            STRICT_SAMPLE_LIMIT
        } else {
            SAMPLE_LIMIT
        };
        let sample = if ids.len() <= limit {
            ids
        } else {
            (0..limit).map(|i| ids[i * ids.len() / limit]).collect()
        };
        Ok(Self {
            actionable: count_actionable(&doc.root),
            doc,
            sample,
            strict,
        })
    }

    /// Decode the base64 `content` string returned by the CDP method.
    pub fn from_base64(content: &str, strict: bool) -> Result<Self, String> {
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(content)
            .map_err(|e| format!("invalid base64: {e}"))?;
        Self::new(&bytes, strict)
    }

    /// Backend node ids to probe against the live DOM.
    pub fn sample_ids(&self) -> &[i64] {
        &self.sample
    }

    pub fn unread_fields(&self) -> usize {
        self.doc.unread_fields
    }

    /// Apply the checks that compare the payload with the live page.
    pub fn accept(self, facts: PageFacts) -> Result<Doc, String> {
        if self.actionable == 0 && facts.dom_interactive > 0 {
            return Err(format!(
                "no actionable nodes, but the page has {} interactive elements",
                facts.dom_interactive
            ));
        }
        let sampled = self.sample.len();
        let too_many = if self.strict {
            facts.unresolved > 0
        } else {
            facts.unresolved * 100 > sampled * MAX_UNRESOLVED_PERCENT
        };
        if too_many {
            return Err(format!(
                "{} of {sampled} sampled node ids do not resolve",
                facts.unresolved
            ));
        }
        Ok(self.doc)
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default)]
pub struct RenderOptions {
    pub interactive: bool,
    pub viewport: bool,
    pub depth: Option<usize>,
    pub urls: bool,
}

/// One rendered line, kept as data until refs are assigned.
#[derive(Debug, Clone, Default)]
pub struct Item {
    pub indent: usize,
    pub role: String,
    pub name: String,
    pub attrs: Vec<String>,
    pub value: Option<String>,
    /// Backend node id when this line should receive a ref.
    pub target: Option<i64>,
    /// Role stored with the ref (used when a stale ref is re-resolved by role and name).
    pub ref_role: String,
    pub ref_id: Option<String>,
    pub url: Option<String>,
    /// Set on iframe lines. The lines indented under an iframe are its content.
    pub frame: Option<FrameInfo>,
    /// Snapshot text of the frame's content from another source (the AX tree of an
    /// out-of-process frame), printed under the iframe line.
    pub nested: Option<String>,
}

/// What the renderer knows about an iframe line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FrameInfo {
    /// Same origin as the main frame all the way down, so APC refs inside it are usable.
    pub same_origin: bool,
    /// The iframe element lives in the page's own process (not inside a read-only frame), so its
    /// backend node id resolves in the page session.
    pub top: bool,
}

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn collect_text(node: &Node, out: &mut Vec<String>) {
    if let Some(t) = node.text.as_deref().or(node.caption.as_deref()) {
        let t = collapse(t);
        if !t.is_empty() {
            out.push(t);
        }
    }
    for child in &node.children {
        collect_text(child, out);
    }
}

fn text_of(node: &Node) -> String {
    let mut parts = Vec::new();
    collect_text(node, &mut parts);
    parts.join(" ")
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}

fn is_strong_reason(reason: u32) -> bool {
    !matches!(
        reason,
        REASON_MOUSE_HOVER | REASON_HOVER_PSEUDO_CLASS | REASON_MOUSE_OVER
    )
}

fn control_kind(node: &Node) -> Option<u32> {
    node.control.as_ref().map(|c| c.kind)
}

fn is_skipped_control(node: &Node) -> bool {
    matches!(
        control_kind(node),
        Some(CONTROL_INPUT_HIDDEN | CONTROL_FIELDSET | CONTROL_OUTPUT)
    )
}

fn outer_is_empty(node: &Node) -> bool {
    node.outer.is_some_and(|r| r.is_empty())
}

/// True when the node is something an agent can act on.
pub fn is_actionable(node: &Node) -> bool {
    if node.dom_id.is_none() || is_skipped_control(node) || outer_is_empty(node) {
        return false;
    }
    let is_control = node.kind == KIND_FORM_CONTROL;
    let Some(info) = &node.info else {
        // Offscreen controls and anchors can arrive without interaction info.
        return is_control
            || (node.kind == KIND_ANCHOR && node.url.as_deref().is_some_and(|u| !u.is_empty()));
    };
    if info
        .disabled_reasons
        .contains(&DISABLED_POINTER_EVENTS_NONE)
    {
        return false;
    }
    if info.disabled && !is_control {
        return false;
    }
    is_control || info.tabbable || info.reasons.iter().any(|r| is_strong_reason(*r))
}

/// Containers that only carry a click listener (for example an app root with a delegated
/// handler) are not click targets when something inside them is. Controls and anchors always are.
fn is_target(node: &Node) -> bool {
    if !is_actionable(node) {
        return false;
    }
    let generic =
        node.kind != KIND_FORM_CONTROL && node.kind != KIND_ANCHOR && role_for(node) == "clickable";
    !generic || !node.children.iter().any(has_actionable)
}

fn has_actionable(node: &Node) -> bool {
    is_target(node) || node.children.iter().any(has_actionable)
}

fn role_for(node: &Node) -> &'static str {
    match node.aria_role {
        9 | 137 | 175 => return "button",
        14 => return "checkbox",
        20 | 21 | 171 | 209 => return "combobox",
        110 => return "link",
        112 => return "listbox",
        113 | 127 => return "option",
        124 => return "menuitem",
        125 => return "menuitemcheckbox",
        126 => return "menuitemradio",
        141 => return "radio",
        153 => return "searchbox",
        155 => return "slider",
        156 => return "spinbutton",
        163 => return "switch",
        164 => return "tab",
        170 => return "textbox",
        180 => return "treeitem",
        _ => {}
    }
    match control_kind(node) {
        Some(CONTROL_CHECKBOX) => "checkbox",
        Some(CONTROL_RADIO) => "radio",
        Some(CONTROL_NUMBER) => "spinbutton",
        Some(CONTROL_SEARCH) => "searchbox",
        Some(CONTROL_SELECT_ONE) => "combobox",
        Some(CONTROL_SELECT_MULTIPLE) => "listbox",
        Some(CONTROL_RANGE) => "slider",
        Some(
            CONTROL_BUTTON_BUTTON
            | CONTROL_BUTTON_SUBMIT
            | CONTROL_BUTTON_RESET
            | CONTROL_BUTTON_POPOVER
            | CONTROL_INPUT_BUTTON
            | CONTROL_INPUT_FILE
            | CONTROL_INPUT_IMAGE
            | CONTROL_INPUT_RESET
            | CONTROL_INPUT_SUBMIT,
        ) => "button",
        Some(_) => "textbox",
        None if node.kind == KIND_ANCHOR => "link",
        None => "clickable",
    }
}

fn is_button_like(role: &str) -> bool {
    matches!(role, "button" | "link" | "clickable" | "tab" | "menuitem")
}

struct Walker<'a> {
    opts: &'a RenderOptions,
    labels: HashMap<(u32, i64), String>,
    items: Vec<Item>,
    /// Depth of enclosing actionable or consumed nodes whose text is already in their name.
    suppress_text: usize,
}

fn collect_labels(node: &Node, labels: &mut HashMap<(u32, i64), String>) {
    if let Some(target) = node.label_for {
        let t = text_of(node);
        if !t.is_empty() {
            labels.entry((node.scope, target)).or_insert(t);
        }
    }
    for child in &node.children {
        collect_labels(child, labels);
    }
}

impl Walker<'_> {
    fn name_for(&self, node: &Node, role: &str) -> String {
        let linked = node
            .dom_id
            .and_then(|id| self.labels.get(&(node.scope, id)))
            .cloned();
        let label = node
            .label
            .as_deref()
            .map(collapse)
            .filter(|s| !s.is_empty());
        let control = node.control.as_ref();
        let placeholder = control
            .map(|c| collapse(&c.placeholder))
            .filter(|s| !s.is_empty());
        let has_value = control.is_some_and(|c| !c.value.is_empty());
        let is_select = matches!(
            control.map(|c| c.kind),
            Some(CONTROL_SELECT_ONE | CONTROL_SELECT_MULTIPLE)
        );
        // For selects Chrome's label can be the selected option's text, which is a value, not a name.
        let label = label.filter(|l| {
            !is_select
                || !control.is_some_and(|c| {
                    c.options
                        .iter()
                        .any(|o| o.selected && collapse(&o.text) == *l)
                })
        });
        let content = if (has_value || is_select) && !is_button_like(role) {
            String::new()
        } else {
            text_of(node)
        };
        let name = linked
            .or(label)
            .or(placeholder)
            .or_else(|| Some(content).filter(|s| !s.is_empty()))
            .or_else(|| {
                control
                    .filter(|_| is_button_like(role))
                    .map(|c| collapse(&c.value))
                    .filter(|s| !s.is_empty())
            })
            .unwrap_or_default();
        if role == "clickable" {
            clip(&name, CLICKABLE_NAME_CHARS)
        } else {
            name
        }
    }

    fn actionable_item(&self, node: &Node, indent: usize) -> Item {
        let role = role_for(node);
        let mut attrs = Vec::new();
        let control = node.control.as_ref();
        if let Some(c) = control {
            if matches!(c.kind, CONTROL_CHECKBOX | CONTROL_RADIO) {
                attrs.push(format!("checked={}", c.checked));
            }
        }
        if let Some(info) = &node.info {
            if info.reasons.contains(&REASON_ARIA_EXPANDED_TRUE) {
                attrs.push("expanded=true".into());
            } else if info.reasons.contains(&REASON_ARIA_EXPANDED_FALSE) {
                attrs.push("expanded=false".into());
            }
            if info.disabled {
                attrs.push("disabled".into());
            }
            if info
                .disabled_reasons
                .contains(&DISABLED_VISUALLY_UNREACHABLE)
            {
                attrs.push("unreachable".into());
            }
        }
        if let Some(c) = control {
            if c.required {
                attrs.push("required".into());
            }
            if c.readonly {
                attrs.push("readonly".into());
            }
            if !c.options.is_empty() {
                let list: Vec<&str> = c
                    .options
                    .iter()
                    .map(|o| {
                        if o.text.is_empty() {
                            o.value.as_str()
                        } else {
                            o.text.as_str()
                        }
                    })
                    .collect();
                attrs.push(format!("options=[{}]", list.join("|")));
            }
        }
        let value = control.and_then(|c| {
            if c.kind == CONTROL_SELECT_ONE || c.kind == CONTROL_SELECT_MULTIPLE {
                let selected: Vec<&str> = c
                    .options
                    .iter()
                    .filter(|o| o.selected)
                    .map(|o| {
                        if o.text.is_empty() {
                            o.value.as_str()
                        } else {
                            o.text.as_str()
                        }
                    })
                    .collect();
                Some(selected.join(", "))
            } else if matches!(c.kind, CONTROL_CHECKBOX | CONTROL_RADIO) {
                None
            } else {
                Some(c.value.clone())
            }
        });
        let name = self.name_for(node, role);
        let value = value.filter(|v| !v.is_empty() && *v != name);
        let target = if node.read_only { None } else { node.dom_id };
        if node.read_only {
            attrs.push("read-only".into());
        }
        Item {
            indent,
            role: role.to_string(),
            name,
            attrs,
            value,
            target,
            ref_role: if role == "clickable" {
                "generic".into()
            } else {
                role.to_string()
            },
            ref_id: None,
            url: if self.opts.urls && role == "link" {
                node.url.clone()
            } else {
                None
            },
            frame: None,
            nested: None,
        }
    }

    fn visible(&self, node: &Node, parent_visible: bool) -> bool {
        if !self.opts.viewport {
            return true;
        }
        match (&node.visible, &node.outer) {
            (Some(v), _) => !v.is_empty(),
            (None, Some(_)) => false,
            (None, None) => parent_visible,
        }
    }

    fn push(&mut self, item: Item) {
        self.items.push(item);
    }

    fn walk(&mut self, node: &Node, indent: usize, parent_visible: bool) {
        let visible = self.visible(node, parent_visible);
        let over_depth = self.opts.depth.is_some_and(|d| indent > d);
        let mut child_indent = indent;
        let mut consumed = false;

        if node.kind == KIND_TEXT {
            if !self.opts.interactive && self.suppress_text == 0 && visible && !over_depth {
                if let Some(t) = node.text.as_deref().map(collapse).filter(|s| !s.is_empty()) {
                    self.push(Item {
                        indent,
                        role: "text".into(),
                        name: t,
                        ..Item::default()
                    });
                }
            }
            return;
        }

        let target = is_target(node);
        if target && !node.read_only {
            if visible && !over_depth {
                let item = self.actionable_item(node, indent);
                self.push(item);
                child_indent += 1;
            }
            // A control's children are its value text and inner editor; they add nothing.
            if node.kind != KIND_FORM_CONTROL {
                self.suppress_text += 1;
                for child in &node.children {
                    self.walk(child, child_indent, visible);
                }
                self.suppress_text -= 1;
            }
            return;
        }

        if target && visible && !over_depth {
            // Read-only frame content: listed so the agent knows it exists, never given a ref.
            let item = self.actionable_item(node, indent);
            self.push(item);
            child_indent += 1;
            // A control's children are its value text and inner editor; they add nothing.
            if node.kind != KIND_FORM_CONTROL {
                self.suppress_text += 1;
                for child in &node.children {
                    self.walk(child, child_indent, visible);
                }
                self.suppress_text -= 1;
            }
            return;
        }

        if let Some(frame) = &node.frame {
            if visible && !over_depth {
                let mut attrs = Vec::new();
                if frame.redacted {
                    attrs.push("redacted".to_string());
                } else if frame.opaque || frame.origin.is_none() {
                    attrs.push("opaque-origin".to_string());
                }
                if !frame.same_origin {
                    attrs.push("read-only".to_string());
                }
                let top = !node.read_only;
                self.push(Item {
                    indent,
                    role: "iframe".into(),
                    name: frame.url.clone(),
                    attrs,
                    // The iframe element itself can be addressed from the page session, which
                    // is what `frame @eN` needs.
                    target: if top { node.dom_id } else { None },
                    ref_role: "Iframe".into(),
                    frame: Some(FrameInfo {
                        same_origin: frame.same_origin,
                        top,
                    }),
                    ..Item::default()
                });
                child_indent += 1;
            }
        } else if !self.opts.interactive && visible && !over_depth {
            match node.kind {
                KIND_HEADING | KIND_PARAGRAPH => {
                    if !has_actionable(node) {
                        let t = text_of(node);
                        if !t.is_empty() {
                            self.push(Item {
                                indent,
                                role: if node.kind == KIND_HEADING {
                                    "heading"
                                } else {
                                    "paragraph"
                                }
                                .into(),
                                name: t,
                                ..Item::default()
                            });
                        }
                        consumed = true;
                    }
                }
                KIND_IMAGE => {
                    if let Some(c) = node
                        .caption
                        .as_deref()
                        .map(collapse)
                        .filter(|s| !s.is_empty())
                    {
                        self.push(Item {
                            indent,
                            role: "img".into(),
                            name: c,
                            ..Item::default()
                        });
                    }
                }
                KIND_CANVAS => self.push(Item {
                    indent,
                    role: "canvas".into(),
                    ..Item::default()
                }),
                KIND_FORM | KIND_TABLE | KIND_TABLE_ROW | KIND_TABLE_CELL | KIND_ORDERED_LIST
                | KIND_UNORDERED_LIST | KIND_LIST_ITEM | KIND_DIALOG_MODAL
                | KIND_DIALOG_MODELESS => {
                    let role = match node.kind {
                        KIND_FORM => "form",
                        KIND_TABLE => "table",
                        KIND_TABLE_ROW => "row",
                        KIND_TABLE_CELL => "cell",
                        KIND_ORDERED_LIST | KIND_UNORDERED_LIST => "list",
                        KIND_LIST_ITEM => "listitem",
                        _ => "dialog",
                    };
                    self.push(Item {
                        indent,
                        role: role.into(),
                        name: node.form_name.clone().unwrap_or_default(),
                        ..Item::default()
                    });
                    child_indent += 1;
                }
                _ => {}
            }
        }

        if consumed {
            return;
        }
        for child in &node.children {
            self.walk(child, child_indent, visible);
        }
    }
}

/// Render the document into lines (refs not yet assigned).
pub fn render_items(doc: &Doc, opts: &RenderOptions) -> Vec<Item> {
    let mut labels = HashMap::new();
    collect_labels(&doc.root, &mut labels);
    let mut walker = Walker {
        opts,
        labels,
        items: Vec::new(),
        suppress_text: 0,
    };
    walker.walk(&doc.root, 0, true);
    walker.items
}

/// Format rendered items as text, one line each.
pub fn format_items(items: &[Item]) -> String {
    let mut out = String::new();
    for item in items {
        out.push_str(&"  ".repeat(item.indent));
        out.push_str("- ");
        out.push_str(&item.role);
        if !item.name.is_empty() {
            out.push(' ');
            out.push_str(&serde_json::to_string(&item.name).unwrap_or_default());
        }
        let mut attrs = item.attrs.clone();
        if let Some(ref_id) = &item.ref_id {
            attrs.push(format!("ref={ref_id}"));
        }
        if let Some(url) = &item.url {
            attrs.push(format!("url={url}"));
        }
        if !attrs.is_empty() {
            out.push_str(&format!(" [{}]", attrs.join(", ")));
        }
        if let Some(value) = &item.value {
            out.push_str(": ");
            out.push_str(value);
        }
        out.push('\n');
        if let Some(nested) = &item.nested {
            for line in nested.lines() {
                out.push_str(&"  ".repeat(item.indent + 1));
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    out
}

/// Index range of the lines rendered under the iframe line at `idx`.
pub fn frame_span(items: &[Item], idx: usize) -> std::ops::Range<usize> {
    let indent = items[idx].indent;
    let end = items[idx + 1..]
        .iter()
        .position(|i| i.indent <= indent)
        .map_or(items.len(), |n| idx + 1 + n);
    idx + 1..end
}

#[cfg(test)]
mod tests {
    use super::*;

    const FORM: &[u8] = include_bytes!("test_fixtures/apc_form.bin");
    const SPA: &[u8] = include_bytes!("test_fixtures/apc_spa.bin");

    fn render(bytes: &[u8], opts: RenderOptions) -> (Doc, Vec<Item>) {
        let doc = decode(bytes).expect("fixture decodes");
        let items = render_items(&doc, &opts);
        (doc, items)
    }

    fn interactive() -> RenderOptions {
        RenderOptions {
            interactive: true,
            ..RenderOptions::default()
        }
    }

    #[test]
    fn decodes_recorded_payload() {
        let doc = decode(FORM).unwrap();
        assert_eq!(doc.main_origin.as_deref(), Some("https://demoqa.com"));
        assert!(doc.viewport.is_some_and(|v| v.width > 0 && v.height > 0));
        assert_eq!(doc.root.kind, 1, "root is CONTENT_ATTRIBUTE_ROOT");
    }

    #[test]
    fn names_radios_and_checkboxes_from_label_nodes() {
        let (_, items) = render(FORM, interactive());
        let find = |role: &str, name: &str| {
            items
                .iter()
                .find(|i| i.role == role && i.name == name)
                .unwrap_or_else(|| panic!("missing {role} {name}"))
        };
        assert_eq!(find("radio", "Male").target, Some(93));
        assert_eq!(find("radio", "Female").target, Some(96));
        assert_eq!(find("checkbox", "Sports").target, Some(137));
        assert_eq!(find("checkbox", "Music").target, Some(143));
        assert!(find("radio", "Male")
            .attrs
            .contains(&"checked=false".to_string()));
        assert!(find("radio", "Male")
            .attrs
            .contains(&"required".to_string()));
    }

    #[test]
    fn maps_controls_to_backend_node_ids() {
        let (_, items) = render(FORM, interactive());
        let first = items.iter().find(|i| i.name == "First Name").unwrap();
        assert_eq!(first.role, "textbox");
        assert_eq!(first.target, Some(74));
        let submit = items
            .iter()
            .find(|i| i.role == "button" && i.name == "Submit")
            .unwrap();
        assert_eq!(submit.target, Some(194));
        assert!(
            items.iter().all(|i| i.indent == 0),
            "interactive output is flat here"
        );
    }

    #[test]
    fn formats_like_the_ax_snapshot() {
        let (_, mut items) = render(FORM, interactive());
        let submit = items.iter_mut().find(|i| i.name == "Submit").unwrap();
        submit.ref_id = Some("e7".into());
        let text = format_items(&items);
        assert!(text.contains("- button \"Submit\" [ref=e7]\n"), "{text}");
        assert!(
            text.contains("- radio \"Male\" [checked=false, required]\n"),
            "{text}"
        );
    }

    #[test]
    fn full_render_keeps_headings_and_text() {
        let (_, items) = render(FORM, RenderOptions::default());
        assert!(items
            .iter()
            .any(|i| i.role == "heading" && i.name == "Practice Form"));
        assert!(items.iter().any(|i| i.role == "text" && i.name == "Gender"));
        let interactive_only = render(FORM, interactive()).1;
        assert!(items.len() > interactive_only.len());
    }

    #[test]
    fn viewport_filter_drops_offscreen_nodes() {
        let (_, all) = render(FORM, interactive());
        let (_, visible) = render(
            FORM,
            RenderOptions {
                viewport: true,
                ..interactive()
            },
        );
        assert!(!visible.is_empty());
        assert!(
            visible.len() < all.len(),
            "{} vs {}",
            visible.len(),
            all.len()
        );
        assert!(visible.iter().any(|i| i.name == "First Name"));
        assert!(
            !visible.iter().any(|i| i.name == "Submit"),
            "Submit is below the fold"
        );
    }

    #[test]
    fn spa_fixture_renders_actionables() {
        let (_doc, items) = render(SPA, interactive());
        assert!(items.iter().any(|i| i.target.is_some()));
    }

    #[test]
    fn truncated_payload_is_an_error() {
        assert!(decode(&FORM[..FORM.len() / 2]).is_err());
        assert!(decode(&[]).is_err());
        assert!(decode(&[0x0a]).is_err());
    }

    #[test]
    fn cross_origin_frames_are_read_only() {
        // root > iframe(cross-origin) > button(dom 5)
        fn tag(n: u32, wire: u32) -> Vec<u8> {
            let mut v = (n << 3) | wire;
            let mut out = Vec::new();
            while v >= 0x80 {
                out.push((v & 0x7f) as u8 | 0x80);
                v >>= 7;
            }
            out.push(v as u8);
            out
        }
        fn field(n: u32, payload: &[u8]) -> Vec<u8> {
            let mut out = tag(n, 2);
            out.push(payload.len() as u8);
            out.extend_from_slice(payload);
            out
        }
        fn varint(n: u32, v: u8) -> Vec<u8> {
            let mut out = tag(n, 0);
            out.push(v);
            out
        }
        let origin = |s: &str| field(2, s.as_bytes());
        let frame_data = |o: &str| {
            let mut fd = field(1, &origin(o));
            fd.extend(field(4, b"https://other.test/"));
            fd
        };
        let button = {
            let mut attrs = varint(2, 5);
            attrs.extend(varint(3, 24));
            attrs.extend(field(16, b"Zoom"));
            attrs.extend(field(21, &[0x78, 0x01]));
            let mut ctrl = varint(3, 16);
            ctrl.extend(field(1, b"z"));
            attrs.extend(field(20, &ctrl));
            field(2, &attrs)
        };
        let iframe = {
            let mut attrs = varint(2, 4);
            attrs.extend(varint(3, 3));
            attrs.extend(field(9, &field(3, &frame_data("https://other.test"))));
            let mut node = field(2, &attrs);
            node.extend(field(1, &button));
            node
        };
        let root = {
            let attrs = varint(3, 1);
            let mut node = field(2, &attrs);
            node.extend(field(1, &iframe));
            node
        };
        let mut payload = field(2, &root);
        payload.extend(field(3, &field(1, &origin("https://main.test"))));
        let doc = decode(&payload).unwrap();
        let items = render_items(&doc, &interactive());
        let zoom = items.iter().find(|i| i.name == "Zoom").expect("listed");
        assert_eq!(zoom.target, None, "no ref inside a cross-origin frame");
        assert!(zoom.attrs.contains(&"read-only".to_string()));
        let frame = items
            .iter()
            .find(|i| i.role == "iframe")
            .expect("frame line");
        assert_eq!(
            frame.frame,
            Some(FrameInfo {
                same_origin: false,
                top: true
            })
        );
        assert_eq!(
            frame.target,
            Some(4),
            "the iframe element itself is addressable"
        );
        assert_eq!(frame_span(&items, 0), 1..items.len());
    }

    #[test]
    fn frame_span_covers_the_indented_lines() {
        let line = |indent: usize, role: &str| Item {
            indent,
            role: role.into(),
            ..Item::default()
        };
        let items = vec![
            line(0, "iframe"),
            line(1, "button"),
            line(1, "iframe"),
            line(2, "textbox"),
            line(0, "link"),
        ];
        assert_eq!(frame_span(&items, 0), 1..4);
        assert_eq!(frame_span(&items, 2), 3..4);
        assert_eq!(frame_span(&items, 4), 5..5);
    }

    #[test]
    fn nested_text_prints_under_the_iframe_line() {
        let items = vec![
            Item {
                indent: 1,
                role: "iframe".into(),
                name: "https://other.test/".into(),
                ref_id: Some("e2".into()),
                nested: Some("- button \"Pay\" [ref=e3]\n  - text".into()),
                ..Item::default()
            },
            Item {
                indent: 1,
                role: "link".into(),
                name: "Next".into(),
                ..Item::default()
            },
        ];
        assert_eq!(
            format_items(&items),
            "  - iframe \"https://other.test/\" [ref=e2]\n    - button \"Pay\" [ref=e3]\n      - text\n  - link \"Next\"\n"
        );
    }

    const CHROME: &str = "Chrome/157.0.8103.39";
    const HEALTHY_PAGE: PageFacts = PageFacts {
        dom_interactive: 12,
        unresolved: 0,
    };

    /// The decision `take_snapshot` makes: the ax fallback warning, or the accepted document.
    fn vet(
        bytes: &[u8],
        chrome: &str,
        facts: impl FnOnce(&Vetted) -> PageFacts,
    ) -> Result<Doc, String> {
        let warn = |reason: String| fallback_warning(SnapshotSource::Apc, chrome, &reason);
        let vetted = Vetted::new(bytes, !is_verified_version(chrome)).map_err(warn)?;
        let facts = facts(&vetted);
        vetted.accept(facts).map_err(warn)
    }

    fn assert_fallback(result: Result<Doc, String>, reason: &str) {
        let warning = result.expect_err("expected the ax fallback");
        assert!(
            warning.starts_with(&format!("# warning: --source apc unavailable ({CHROME}): ")),
            "{warning}"
        );
        assert!(warning.contains(reason), "{warning}");
        assert!(warning.ends_with("; using the ax snapshot"), "{warning}");
        assert!(!warning.contains('\n'), "one line: {warning}");
    }

    fn put_varint(out: &mut Vec<u8>, mut v: u64) {
        while v >= 0x80 {
            out.push((v as u8 & 0x7f) | 0x80);
            v >>= 7;
        }
        out.push(v as u8);
    }

    fn length_delimited(field: u32, body: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        put_varint(&mut out, u64::from(field) << 3 | 2);
        put_varint(&mut out, body.len() as u64);
        out.extend_from_slice(body);
        out
    }

    fn varint_field(field: u32, v: u64) -> Vec<u8> {
        let mut out = Vec::new();
        put_varint(&mut out, u64::from(field) << 3);
        put_varint(&mut out, v);
        out
    }

    type Rewriter<'a> = dyn Fn(u32, &[u8], &[u8]) -> Vec<u8> + 'a;

    /// Re-encode `buf`, letting `f` replace any field. `f` gets the field number, its payload
    /// and the raw bytes, and returns the replacement bytes.
    fn rewrite(buf: &[u8], f: &Rewriter) -> Vec<u8> {
        let mut out = Vec::new();
        let mut fields = Fields::new(buf);
        while fields.pos < buf.len() {
            let start = fields.pos;
            let (field, wire) = fields.read().unwrap();
            let raw = &buf[start..fields.pos];
            let payload = match wire {
                Wire::Bytes(b) => b,
                _ => &[],
            };
            out.extend(f(field, payload, raw));
        }
        out
    }

    /// Move a ContentAttributes field to another field number in every node of the payload.
    fn shift_attribute_field(payload: &[u8], from: u32, to: u32) -> Vec<u8> {
        fn node(buf: &[u8], from: u32, to: u32) -> Vec<u8> {
            rewrite(buf, &|field, body, raw| match field {
                1 => length_delimited(1, &node(body, from, to)),
                2 => length_delimited(
                    2,
                    &rewrite(body, &|f, _, raw| {
                        if f != from {
                            return raw.to_vec();
                        }
                        let mut tag_len = 0;
                        read_varint(raw, &mut tag_len).unwrap();
                        let mut out = Vec::new();
                        put_varint(&mut out, u64::from(to) << 3 | (u64::from(raw[0]) & 7));
                        out.extend_from_slice(&raw[tag_len..]);
                        out
                    }),
                ),
                _ => raw.to_vec(),
            })
        }
        rewrite(payload, &|field, body, raw| {
            if field == 2 {
                length_delimited(2, &node(body, from, to))
            } else {
                raw.to_vec()
            }
        })
    }

    /// A payload with a valid root and viewport but no content under the root.
    fn empty_tree() -> Vec<u8> {
        let attributes = varint_field(3, u64::from(KIND_ROOT));
        let root = length_delimited(2, &attributes);
        let mut viewport = varint_field(3, 1280);
        viewport.extend(varint_field(4, 720));
        let mut payload = length_delimited(2, &root);
        payload.extend(length_delimited(5, &viewport));
        payload
    }

    #[test]
    fn healthy_payload_does_not_fall_back() {
        for payload in [FORM, SPA] {
            let doc = vet(payload, CHROME, |_| HEALTHY_PAGE).expect("healthy payload is accepted");
            assert!(
                doc.unread_fields > 0,
                "fixtures carry fields the decoder skips"
            );
        }
        let strict = vet(FORM, "Chrome/999.0.0.0", |_| HEALTHY_PAGE);
        assert!(
            strict.is_ok(),
            "unverified version still uses apc when healthy"
        );
    }

    #[test]
    fn truncated_payload_falls_back() {
        let cut = &FORM[..FORM.len() / 2];
        assert_fallback(
            vet(cut, CHROME, |_| HEALTHY_PAGE),
            "could not decode annotated page content",
        );
        assert_fallback(
            vet(&FORM[..FORM.len() - 1], CHROME, |_| HEALTHY_PAGE),
            "could not decode annotated page content",
        );
    }

    #[test]
    fn shifted_attribute_type_is_an_unknown_structure() {
        let shifted = shift_attribute_field(FORM, 3, 43);
        assert_fallback(
            vet(&shifted, CHROME, |_| HEALTHY_PAGE),
            "unexpected root node type",
        );
        let before = decode(FORM).unwrap().unread_fields;
        let after = decode(&shifted).unwrap().unread_fields;
        assert!(after > before, "shifted fields are counted as unread");
    }

    #[test]
    fn shifted_node_id_leaves_no_actionable_nodes() {
        let shifted = shift_attribute_field(FORM, 2, 42);
        assert_fallback(
            vet(&shifted, CHROME, |_| HEALTHY_PAGE),
            "no actionable nodes, but the page has 12 interactive elements",
        );
    }

    #[test]
    fn empty_tree_on_a_page_with_buttons_falls_back() {
        let payload = empty_tree();
        assert_fallback(
            vet(&payload, CHROME, |_| PageFacts {
                dom_interactive: 3,
                unresolved: 0,
            }),
            "no actionable nodes, but the page has 3 interactive elements",
        );
        let blank = vet(&payload, CHROME, |_| PageFacts::default());
        assert!(blank.is_ok(), "an empty tree on an empty page is healthy");
    }

    #[test]
    fn unresolved_node_ids_fall_back() {
        assert_fallback(
            vet(FORM, CHROME, |v| PageFacts {
                dom_interactive: 12,
                unresolved: v.sample_ids().len(),
            }),
            "sampled node ids do not resolve",
        );
    }

    #[test]
    fn one_unresolved_id_is_tolerated_only_for_verified_versions() {
        let one = |_: &Vetted| PageFacts {
            dom_interactive: 12,
            unresolved: 1,
        };
        assert!(vet(FORM, CHROME, one).is_ok());
        let strict = vet(FORM, "Chrome/999.0.0.0", one).err().unwrap();
        assert!(strict.contains("1 of "), "{strict}");
        assert!(strict.contains("(Chrome/999.0.0.0)"), "{strict}");
    }

    #[test]
    fn chrome_versions() {
        assert_eq!(chrome_major("Chrome/157.0.8103.39"), Some(157));
        assert_eq!(chrome_major("HeadlessChrome/157.0.8103.39"), Some(157));
        assert_eq!(chrome_major("unknown"), None);
        assert!(is_verified_version("Chrome/157.0.8103.39"));
        assert!(!is_verified_version("Chrome/158.0.1.1"));
        assert!(!is_verified_version("not a version"));
    }

    #[test]
    fn source_parses() {
        assert_eq!(SnapshotSource::parse("ax"), Some(SnapshotSource::Ax));
        assert_eq!(SnapshotSource::parse("apc"), Some(SnapshotSource::Apc));
        assert_eq!(SnapshotSource::parse("hybrid"), None);
        assert_eq!(SnapshotSource::parse("dom"), None);
        assert_eq!(SnapshotSource::default().as_str(), "ax");
    }
}
