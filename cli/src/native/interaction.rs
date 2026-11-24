use std::collections::HashMap;

use serde_json::Value;

use super::cdp::client::CdpClient;
use super::cdp::types::*;
use super::element::{
    resolve_element_center, resolve_element_object_id, session_viewport_offset, RefMap,
};
use super::pointer::{ClickSpec, Modifiers, MouseButton};

/// Outcome of a click. `dialog_opened` is true if a JavaScript dialog opened
/// mid-sequence (the page is then blocked until `dialog accept`/`dismiss`).
/// `pending_release` is set only when the dialog opened after mousePressed but
/// before mouseReleased: the button is logically held until the caller
/// dispatches the release (done once the dialog is resolved), otherwise the
/// next click would register as a drag or double-click.
#[derive(Default)]
pub struct ClickResult {
    /// Final pointer position in the top-level page viewport, including dialogs.
    pub position: (f64, f64),
    pub dialog_opened: bool,
    pub pending_release: Option<PendingRelease>,
    pub x: f64,
    pub y: f64,
    pub button_pressed: bool,
}

pub struct PendingRelease {
    pub session_id: String,
    pub x: f64,
    pub y: f64,
    pub button: MouseButton,
    pub click_count: u8,
    pub modifiers: Modifiers,
}

pub async fn click(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    spec: ClickSpec,
    iframe_sessions: &HashMap<String, String>,
) -> Result<ClickResult, String> {
    click_element(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        spec,
        iframe_sessions,
        true,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn click_element(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    spec: ClickSpec,
    iframe_sessions: &HashMap<String, String>,
    require_actionable: bool,
) -> Result<ClickResult, String> {
    let (x, y, effective_session_id) = resolve_element_center(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
        require_actionable,
    )
    .await?;
    // A click-triggered dialog can fire on the frame's own session (OOPIF) or
    // on the top-level page session; both count as "ours". A dialog on any
    // other session belongs to a background tab and must not abort this click.
    let offset =
        session_viewport_offset(client, session_id, &effective_session_id, iframe_sessions).await?;
    let mut result = dispatch_click(
        client,
        &effective_session_id,
        &[effective_session_id.as_str(), session_id],
        x,
        y,
        spec,
    )
    .await?;
    // Compute before dispatch: a click may navigate or open a blocking dialog.
    result.position = (x + offset.0, y + offset.1);
    (result.x, result.y) = result.position;
    Ok(result)
}

pub async fn hover(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<(f64, f64), String> {
    let (x, y, effective_session_id) = resolve_element_center(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
        false,
    )
    .await?;
    let offset =
        session_viewport_offset(client, session_id, &effective_session_id, iframe_sessions).await?;
    client
        .send_command_typed::<_, Value>(
            "Input.dispatchMouseEvent",
            &DispatchMouseEventParams {
                event_type: "mouseMoved".to_string(),
                x,
                y,
                button: None,
                buttons: None,
                click_count: None,
                delta_x: None,
                delta_y: None,
                modifiers: None,
            },
            Some(&effective_session_id),
        )
        .await?;
    Ok((x + offset.0, y + offset.1))
}

/// Stable codes for refusals before acting. An error message that starts with
/// `<code>: ` surfaces the code in the JSON envelope.
pub const FORM_ERROR_CODES: &[&str] = &[
    "not_editable",
    "read_only",
    "disabled",
    "not_checkable",
    "option_not_found",
    "ambiguous",
    "not_multiple",
    "malformed_value",
];

/// Returns the form-action error code an error message starts with.
pub fn form_error_code(error: &str) -> Option<&'static str> {
    let (head, _) = error.split_once(": ")?;
    FORM_ERROR_CODES.iter().copied().find(|c| *c == head)
}

pub fn form_error(code: &str, message: impl std::fmt::Display) -> String {
    format!("{code}: {message}")
}

/// An input or textarea is a text control, found by tag first: it inherits
/// `isContentEditable` from an editable ancestor but uses its value. A select,
/// option or button inside an editable region keeps its own type; it is an
/// editor only when it is itself an editing host. Any other element with
/// `isContentEditable` is an editor.
macro_rules! editor_js {
    () => {
        r#"
        const isTextControl = (n) => n.localName === 'input' || n.localName === 'textarea';
        const isOwnControl = (n) => ['select', 'option', 'optgroup', 'button'].includes(n.localName)
            && n.contentEditable !== 'true' && n.contentEditable !== 'plaintext-only';
        const isEditor = (n) => !isTextControl(n) && !isOwnControl(n) && !!n.isContentEditable;
"#
    };
}

const EDITOR_JS: &str = editor_js!();

/// A label stands for its control, and a part of a date or time input (what a
/// snapshot ref points at) stands for the input.
const RETARGET_JS: &str = concat!(
    editor_js!(),
    r#"
        const host = this.getRootNode && this.getRootNode().host;
        const el = this.localName === 'label' && this.control && !isEditor(this) ? this.control
            : host && host.localName === 'input' ? host : this;
"#
);

/// Decides from the target's own properties whether `fill` or `type` may act.
/// The page only answers with a refusal kind; the message is built in Rust
/// from fixed text, so nothing the page returns reaches it.
const PRECHECK_JS: &str = r#"function(mode) {
        /*RETARGET*/
        const tag = el.localName;
        const type = tag === 'input' ? el.type : '';
        const fail = (refuse) => ({ refuse, tag, type });
        if (el.matches(':disabled')) return fail('disabled');
        // Elements and input types that can never take text, with the command
        // to use instead.
        const neverText = {
            select: 'select', optgroup: 'select', option: 'select',
            button: 'plain', label: 'plain',
        };
        const neverTextInput = {
            checkbox: 'check', radio: 'check',
            file: 'plain', hidden: 'plain', image: 'plain', button: 'plain', submit: 'plain', reset: 'plain',
        };
        const valueTypes = ['color', 'date', 'time', 'datetime-local', 'month', 'week', 'range'];
        let kind = Object.hasOwn(neverText, tag) && !isEditor(el) ? neverText[tag] : null;
        if (kind === null && tag === 'input' && Object.hasOwn(neverTextInput, type)) kind = neverTextInput[type];
        if (kind === null && mode !== 'fill' && tag === 'input' && valueTypes.includes(type)) kind = 'fill';
        if (kind !== null) return fail(kind);
        // Any step that destroys the value is refused on a readonly field:
        // `fill` and `type --clear`. A plain `type` does not clear, so it is
        // left to the page (a trigger may forward focus).
        if (mode !== 'type' && el.readOnly) return fail('read_only');
        return { ok: true };
    }"#;

/// Reads what the form actions need to know about an element.
const PROBE_JS: &str = r#"function() {
        /*RETARGET*/
        const tag = el.localName;
        const type = tag === 'input' ? el.type : '';
        const ariaRoles = ['checkbox', 'radio', 'switch', 'menuitemcheckbox', 'menuitemradio', 'option', 'treeitem'];
        const roleOf = (n) => (n.getAttribute('role') || '').split(/\s+/).find((r) => ariaRoles.includes(r));
        const role = roleOf(el);
        let box = null;
        let state = null;
        if (tag === 'input' && (type === 'checkbox' || type === 'radio')) {
            box = el; state = el.checked;
        } else if (role) {
            box = el;
            state = (role === 'option' || role === 'treeitem') && !el.hasAttribute('aria-checked')
                ? el.getAttribute('aria-selected') === 'true'
                : el.getAttribute('aria-checked') === 'true';
        } else {
            const label = tag === 'label' ? el : el.closest('label');
            const ctrl = label && label.control;
            const sel = 'input[type="checkbox"], input[type="radio"], ' + ariaRoles.slice(0, 5).map((r) => '[role~="' + r + '"]').join(', ');
            const nested = el.querySelector(sel) || (el.shadowRoot && el.shadowRoot.querySelector(sel));
            const inner = ctrl && (ctrl.type === 'checkbox' || ctrl.type === 'radio') ? ctrl : nested;
            if (inner) {
                box = inner;
                state = inner.localName === 'input' ? inner.checked : inner.getAttribute('aria-checked') === 'true';
            }
        }
        const boxRole = box && (roleOf(box) || box.type);
        let value = null;
        if (isEditor(el)) value = el.innerText;
        else if (tag === 'input' || tag === 'textarea') value = el.type === 'password' ? '' : el.value;
        let selected = 0;
        try {
            if (tag === 'input' || tag === 'textarea') selected = Math.max(0, el.selectionEnd - el.selectionStart) || 0;
        } catch (e) {}
        return {
            tag, type,
            valueInput: tag === 'input' && ['color', 'date', 'time', 'datetime-local', 'month', 'week', 'range'].includes(type),
            password: type === 'password',
            maxLength: (tag === 'textarea' || (tag === 'input' && ['text', 'search', 'url', 'tel', 'email', 'password'].includes(type))) && typeof el.maxLength === 'number' ? el.maxLength : -1,
            contentEditable: isEditor(el),
            value,
            selected,
            disabled: el.matches(':disabled'),
            checkable: box !== null,
            checked: state,
            native: !!box && box.localName === 'input',
            radio: boxRole === 'radio' || boxRole === 'menuitemradio',
            boxDisabled: box ? box.matches(':disabled') : false,
        };
    }"#;

const KNOWN_TAGS: &[&str] = &[
    "a", "article", "aside", "button", "canvas", "details", "dialog", "div", "fieldset", "footer",
    "form", "h1", "h2", "h3", "h4", "h5", "h6", "header", "iframe", "img", "input", "label",
    "legend", "li", "main", "nav", "ol", "optgroup", "option", "output", "p", "pre", "section",
    "select", "span", "summary", "table", "td", "textarea", "th", "tr", "ul",
];

const KNOWN_INPUT_TYPES: &[&str] = &[
    "button",
    "checkbox",
    "color",
    "date",
    "datetime-local",
    "email",
    "file",
    "hidden",
    "image",
    "month",
    "number",
    "password",
    "radio",
    "range",
    "reset",
    "search",
    "submit",
    "tel",
    "text",
    "time",
    "url",
    "week",
];

/// Names an element for an error or warning from fixed vocabulary only: a page
/// controls tag names and ids, so anything outside the lists is "<unknown>".
fn describe_target(tag: &str, input_type: &str) -> String {
    if tag == "input" && KNOWN_INPUT_TYPES.contains(&input_type) {
        format!("<input type=\"{input_type}\">")
    } else if KNOWN_TAGS.contains(&tag) {
        format!("<{tag}>")
    } else {
        "<unknown>".to_string()
    }
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct FormProbe {
    tag: String,
    #[serde(rename = "type")]
    input_type: String,
    value_input: bool,
    password: bool,
    max_length: i64,
    content_editable: bool,
    value: Option<String>,
    /// Characters of the value that the next insert replaces.
    selected: i64,
    disabled: bool,
    checkable: bool,
    checked: Option<bool>,
    native: bool,
    radio: bool,
    box_disabled: bool,
}

impl FormProbe {
    /// Stands in when the element cannot be inspected, so the action proceeds
    /// as it would without the checks.
    fn unknown() -> Self {
        Self {
            max_length: -1,
            ..Self::default()
        }
    }

    fn describe(&self) -> String {
        describe_target(&self.tag, &self.input_type)
    }

    fn comparable(&self, text: &str) -> String {
        let text = text.replace("\r\n", "\n");
        let text = text.as_str();
        if self.content_editable {
            text.split_whitespace().collect::<Vec<_>>().join(" ")
        } else if self.input_type == "color" {
            text.to_lowercase()
        } else {
            text.to_string()
        }
    }
}

/// What a read after the action found. Every action reads its result through
/// `read_after`, built on the shared `read_after_action`: a document that is
/// gone, or a target that is no longer in the page, ends the verification
/// without failing an action that already happened.
enum AfterRead<T> {
    Value(T),
    Gone,
    Unreadable,
}

async fn read_after<T>(read: impl std::future::Future<Output = Result<T, String>>) -> AfterRead<T> {
    match super::element::read_after_action(read).await {
        Ok(Some(value)) => AfterRead::Value(value),
        Ok(None) => AfterRead::Gone,
        Err(error) => {
            let lower = error.to_lowercase();
            if [
                "element not found",
                "cannot find object",
                "target navigated",
            ]
            .iter()
            .any(|marker| lower.contains(marker))
            {
                AfterRead::Gone
            } else {
                AfterRead::Unreadable
            }
        }
    }
}

/// The warning for a read that gave no value.
fn unverified_warning<T>(read: &AfterRead<T>) -> Option<String> {
    match read {
        AfterRead::Value(_) => None,
        AfterRead::Gone => Some(super::element::STATE_NOT_VERIFIED.to_string()),
        AfterRead::Unreadable => {
            Some("could not verify the result: the page could not be inspected".to_string())
        }
    }
}

/// The warning for a call that failed after the action was dispatched.
async fn failure_warning(error: String) -> String {
    let read = read_after(std::future::ready(Err::<(), String>(error))).await;
    unverified_warning(&read).unwrap_or_default()
}

/// What a text action reports when the page did not end up as requested. The
/// action is not failed: pages apply values late or rewrite them.
#[derive(Debug, Default)]
pub struct FormOutcome {
    pub warning: Option<String>,
}

async fn call_on(
    client: &CdpClient,
    session_id: &str,
    object_id: &str,
    function_declaration: String,
    argument: Option<Value>,
) -> Result<EvaluateResult, String> {
    let result: EvaluateResult = client
        .send_command_typed(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration,
                object_id: Some(object_id.to_string()),
                arguments: argument.map(|value| {
                    vec![CallArgument {
                        value: Some(value),
                        object_id: None,
                    }]
                }),
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(session_id),
        )
        .await?;
    Ok(result)
}

async fn probe_object(
    client: &CdpClient,
    session_id: &str,
    object_id: &str,
) -> Result<FormProbe, String> {
    let js = PROBE_JS.replace("/*RETARGET*/", RETARGET_JS);
    let result = call_on(client, session_id, object_id, js, None).await?;
    let value = result
        .result
        .value
        .ok_or_else(|| "Could not inspect the element".to_string())?;
    serde_json::from_value(value).map_err(|e| format!("Could not inspect the element: {e}"))
}

async fn probe_selector(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<FormProbe, String> {
    let (object_id, effective_session_id) = resolve_element_object_id(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;
    probe_object(client, &effective_session_id, &object_id).await
}

/// The control a target stands for: a label resolves to its control, and a part
/// of a date or time input to the input. Every step of `fill` and `type` acts
/// on this one object.
async fn resolve_control(client: &CdpClient, session_id: &str, object_id: &str) -> String {
    let js = format!("function() {{ {RETARGET_JS} return el; }}");
    let resolved = client
        .send_command_typed::<_, EvaluateResult>(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: js,
                object_id: Some(object_id.to_string()),
                arguments: None,
                return_by_value: Some(false),
                await_promise: Some(false),
            },
            Some(session_id),
        )
        .await;
    resolved
        .ok()
        .and_then(|r| r.result.object_id)
        .unwrap_or_else(|| object_id.to_string())
}

/// The code and text for a refusal kind. Anything the page sends outside the
/// known kinds gets the plain refusal; the text is always owned here.
fn refusal(kind: &str, verb: &str) -> (&'static str, String) {
    match kind {
        "disabled" => ("disabled", format!("is disabled and cannot be {verb}")),
        "read_only" => ("read_only", format!("is readonly and cannot be {verb}")),
        "select" => ("not_editable", format!("cannot be {verb}; use select")),
        "check" => (
            "not_editable",
            format!("cannot be {verb}; use check or uncheck"),
        ),
        "fill" => (
            "not_editable",
            format!("cannot be {verb}; use fill to set the value of this input"),
        ),
        _ => ("not_editable", format!("cannot be {verb}")),
    }
}

/// Focuses the control. With `guard`, a target that neither receives focus nor
/// gets a focus event (a hidden, inert or non-focusable element) is refused:
/// text would go to whatever was focused before. A target whose focus handler
/// moves focus elsewhere is allowed.
/// Open and closed shadow roots are walked.
async fn focus_target(
    client: &CdpClient,
    session_id: &str,
    object_id: &str,
    target: &str,
    verb: &str,
    guard: bool,
) -> Result<(), String> {
    let js = format!(
        r#"function() {{
        {RETARGET_JS}
        const deep = (d) => {{
            let a = d.activeElement;
            while (a && a.shadowRoot && a.shadowRoot.activeElement) a = a.shadowRoot.activeElement;
            return a;
        }};
        const before = deep(el.ownerDocument);
        let focused = false;
        const seen = (e) => {{ if (e.isTrusted) focused = true; }};
        el.addEventListener('focus', seen, true);
        el.focus();
        el.removeEventListener('focus', seen, true);
        const after = deep(el.ownerDocument);
        let landed = after !== before;
        for (let n = after; n && !landed; n = n.parentNode || n.host) if (n === el) landed = true;
        return {{ landed: landed || focused, tag: el.localName, type: el.localName === 'input' ? el.type : '' }};
    }}"#
    );
    let result = call_on(client, session_id, object_id, js, None).await?;
    let Some(value) = result.result.value.as_ref() else {
        return Ok(());
    };
    if guard && value.get("landed").and_then(Value::as_bool) == Some(false) {
        let field = |key: &str| value.get(key).and_then(Value::as_str).unwrap_or("");
        return Err(form_error(
            "not_editable",
            format!(
                "Element {target} ({}) cannot be {verb}: focusing it does not move focus, so the text would go to whatever was focused before",
                describe_target(field("tag"), field("type"))
            ),
        ));
    }
    Ok(())
}

/// Empties the control so the next insert replaces its content: an input is
/// cleared, an editable region has its content selected and deleted. A field
/// that turned readonly on focus is refused with its value kept.
async fn clear_control(
    client: &CdpClient,
    session_id: &str,
    object_id: &str,
    target: &str,
    verb: &str,
) -> Result<(), String> {
    let js = r#"function() {
        /*EDITOR*/
        if (!isEditor(this) && this.readOnly) {
            return { refuse: 'read_only', tag: this.localName, type: this.localName === 'input' ? this.type : '' };
        }
        if (isEditor(this)) {
            const doc = this.ownerDocument;
            const range = doc.createRange();
            range.selectNodeContents(this);
            const selection = doc.defaultView.getSelection();
            selection.removeAllRanges();
            selection.addRange(range);
            doc.execCommand('delete');
            return;
        }
        this.select && this.select();
        this.value = '';
        this.dispatchEvent(new Event('input', { bubbles: true }));
    }"#
    .replace("/*EDITOR*/", EDITOR_JS);
    let result = call_on(client, session_id, object_id, js, None).await?;
    let Some(value) = result.result.value.as_ref() else {
        return Ok(());
    };
    if value.get("refuse").and_then(Value::as_str) != Some("read_only") {
        return Ok(());
    }
    let field = |key: &str| value.get(key).and_then(Value::as_str).unwrap_or("");
    let (code, reason) = refusal("read_only", verb);
    Err(form_error(
        code,
        format!(
            "Element {target} ({}) {reason}",
            describe_target(field("tag"), field("type"))
        ),
    ))
}

/// Refuses `fill` or `type` on a target that cannot take the text, before
/// anything is changed. The message names the target as the caller passed it.
async fn precheck(
    client: &CdpClient,
    session_id: &str,
    object_id: &str,
    mode: &str,
    target: &str,
) -> Result<(), String> {
    let js = PRECHECK_JS.replace("/*RETARGET*/", RETARGET_JS);
    let result = call_on(
        client,
        session_id,
        object_id,
        js,
        Some(serde_json::json!(mode)),
    )
    .await?;
    let Some(value) = result.result.value.as_ref() else {
        return Ok(());
    };
    let Some(kind) = value.get("refuse").and_then(Value::as_str) else {
        return Ok(());
    };
    let field = |key: &str| value.get(key).and_then(Value::as_str).unwrap_or("");
    let described = describe_target(field("tag"), field("type"));
    let verb = match mode {
        "fill" => "filled",
        "type-clear" => "cleared and typed into",
        _ => "typed into",
    };
    let (code, reason) = refusal(kind, verb);
    Err(form_error(
        code,
        format!("Element {target} ({described}) {reason}"),
    ))
}

struct ValueSet {
    snapped: Option<String>,
    stored: Option<String>,
}

/// Sets date, time, color and range inputs directly: Chrome drops inserted
/// text there. Refuses a value the input would sanitize away. A color input
/// takes any CSS color it stores as that color's `#rrggbb`; one it stores as
/// another color is refused. The result is later compared with the value the
/// browser stored, not the text given.
async fn set_value_input(
    client: &CdpClient,
    session_id: &str,
    object_id: &str,
    value: &str,
) -> Result<ValueSet, String> {
    let js = r#"function(value) {
        /*RETARGET*/
        const type = el.type;
        const minimum = el.min !== '' ? Number(el.min) : 0;
        const maximum = el.max !== '' ? Number(el.max) : 100;
        const text = value.trim();
        const set = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set;
        const old = el.value;
        set.call(el, text);
        const got = el.value;
        set.call(el, old);
        // A range value inside its bounds is set as the browser does, snapped
        // to the step; unparsable or out-of-range values are refused.
        const number = Number(text);
        const colorAccepted = () => {
            const context = el.ownerDocument.createElement('canvas').getContext('2d');
            if (!context) {
                return CSS.supports('color', text) && (got !== '#000000' || /^(black|#000|#000000)$/i.test(text));
            }
            const parse = (sentinel) => {
                context.fillStyle = sentinel;
                context.fillStyle = text;
                return context.fillStyle;
            };
            const parsed = parse('#010203');
            if (parsed !== parse('#040506')) {
                return false;
            }
            const rgb = /^rgba?\((\d+), (\d+), (\d+)/.exec(parsed);
            const hex = parsed.startsWith('#') ? parsed
                : rgb ? '#' + rgb.slice(1, 4).map((c) => Number(c).toString(16).padStart(2, '0')).join('')
                : null;
            return hex !== null ? got === hex : got !== '#000000';
        };
        const accepted = type === 'color' ? colorAccepted()
            : type === 'range' ? /^[-+]?(\d+\.?\d*|\.\d+)(e[-+]?\d+)?$/i.test(text) && number >= minimum && number <= maximum
            : got !== '' || text === '';
        if (!accepted) {
            return { malformed: true, type, minimum, maximum };
        }
        set.call(el, text);
        el.dispatchEvent(new Event('input', { bubbles: true, composed: true }));
        el.dispatchEvent(new Event('change', { bubbles: true }));
        if (type === 'range' && Number(got) !== number) {
            return { snapped: true };
        }
        return { ok: true, stored: got };
    }"#
    .replace("/*RETARGET*/", RETARGET_JS);
    let result = call_on(
        client,
        session_id,
        object_id,
        js,
        Some(serde_json::json!(value)),
    )
    .await?;
    let value = result.result.value.unwrap_or(Value::Null);
    if value.get("malformed").and_then(Value::as_bool) == Some(true) {
        return Err(form_error("malformed_value", malformed_message(&value)));
    }
    Ok(ValueSet {
        snapped: (value.get("snapped").and_then(Value::as_bool) == Some(true)).then(|| {
            "value snapped to the step: the field holds a different value than requested"
                .to_string()
        }),
        stored: value
            .get("stored")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

/// The refusal for a value an input would sanitize away. The page supplies
/// only the input type (checked against a fixed list) and the range bounds
/// (numbers); the text is built here.
fn malformed_message(page: &Value) -> String {
    let kind = page.get("type").and_then(Value::as_str).unwrap_or("");
    let bound = |key: &str, default: f64| {
        let n = page
            .get(key)
            .and_then(Value::as_f64)
            .filter(|n| n.is_finite())
            .unwrap_or(default);
        if n.fract() == 0.0 && n.abs() < 1e15 {
            format!("{}", n as i64)
        } else {
            format!("{n}")
        }
    };
    let (kind, expected) = match kind {
        "color" => (
            "color",
            "a CSS color such as #rrggbb, #rgb or red".to_string(),
        ),
        "date" => ("date", "YYYY-MM-DD".to_string()),
        "time" => ("time", "HH:MM or HH:MM:SS".to_string()),
        "datetime-local" => ("datetime-local", "YYYY-MM-DDTHH:MM".to_string()),
        "month" => ("month", "YYYY-MM".to_string()),
        "week" => ("week", "YYYY-Www".to_string()),
        "range" => (
            "range",
            format!(
                "a number from {} to {}",
                bound("minimum", 0.0),
                bound("maximum", 100.0)
            ),
        ),
        _ => return "Malformed value for this input".to_string(),
    };
    format!("Malformed value for input of type \"{kind}\": expected {expected}")
}

/// Warns when the value is longer than the field's maxlength; the browser
/// keeps only the first characters. The value itself is never echoed.
fn truncation_warning(probe: &FormProbe, value: &str) -> Option<String> {
    let sent = value.encode_utf16().count();
    (probe.max_length >= 0 && sent as i64 > probe.max_length).then(|| {
        format!(
            "value_truncated: {} accepts at most {} characters (maxlength) but {} were given",
            probe.describe(),
            probe.max_length,
            sent
        )
    })
}

/// Warns when the element holds something other than what was sent.
fn fill_warning(after: &FormProbe, sent: &str) -> Option<String> {
    if after.password {
        return None;
    }
    let observed = after.value.clone()?;
    if after.comparable(&observed) == after.comparable(sent) {
        return None;
    }
    if observed.is_empty() {
        return Some(format!(
            "effect_not_observed: {} is empty after fill; the page may apply the value later",
            after.describe()
        ));
    }
    // Field values are never echoed: only lengths.
    let held = observed.encode_utf16().count();
    let given = sent.encode_utf16().count();
    Some(if held != given {
        format!(
            "{} was changed by the page after it was entered (length {given} \u{2192} {held})",
            after.describe()
        )
    } else {
        format!(
            "{} holds a different value than requested",
            after.describe()
        )
    })
}

pub async fn fill(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    value: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<FormOutcome, String> {
    let (object_id, effective_session_id) = resolve_element_object_id(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;
    let object_id = resolve_control(client, &effective_session_id, &object_id).await;

    precheck(
        client,
        &effective_session_id,
        &object_id,
        "fill",
        selector_or_ref,
    )
    .await?;
    let before = probe_object(client, &effective_session_id, &object_id)
        .await
        .unwrap_or_else(|_| FormProbe::unknown());
    if before.value_input {
        let set = match set_value_input(client, &effective_session_id, &object_id, value).await {
            Ok(set) => set,
            Err(error) if form_error_code(&error).is_some() => return Err(error),
            Err(error) => {
                return Ok(FormOutcome {
                    warning: Some(failure_warning(error).await),
                })
            }
        };
        if set.snapped.is_some() {
            return Ok(FormOutcome {
                warning: set.snapped,
            });
        }
        let expected = set.stored.as_deref().unwrap_or(value.trim());
        let read = read_after(probe_object(client, &effective_session_id, &object_id)).await;
        let warning = match &read {
            AfterRead::Value(after) => fill_warning(after, expected),
            other => unverified_warning(other),
        };
        return Ok(FormOutcome { warning });
    }

    focus_target(
        client,
        &effective_session_id,
        &object_id,
        selector_or_ref,
        "filled",
        true,
    )
    .await?;
    clear_control(
        client,
        &effective_session_id,
        &object_id,
        selector_or_ref,
        "filled",
    )
    .await?;

    // Insert text (keyboard input dispatched at page level, use parent session_id)
    client
        .send_command_typed::<_, Value>(
            "Input.insertText",
            &InsertTextParams {
                text: value.to_string(),
            },
            Some(session_id),
        )
        .await?;

    let read = read_after(probe_object(client, &effective_session_id, &object_id)).await;
    let warning = match &read {
        AfterRead::Value(after) => {
            truncation_warning(&before, value).or_else(|| fill_warning(after, value))
        }
        other => unverified_warning(other),
    };
    Ok(FormOutcome { warning })
}

#[allow(clippy::too_many_arguments)]
pub async fn type_text(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    text: &str,
    clear: bool,
    delay_ms: Option<u64>,
    iframe_sessions: &HashMap<String, String>,
) -> Result<FormOutcome, String> {
    let (object_id, effective_session_id) = resolve_element_object_id(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;
    let object_id = resolve_control(client, &effective_session_id, &object_id).await;

    // Control characters alone are key events, which any focused target can
    // receive; any text needs a target that takes it.
    let keys_only = !text.is_empty() && text.chars().all(|c| matches!(c, '\n' | '\r' | '\t'));
    // Clearing destroys the value, so `--clear` is checked like `fill`, even
    // for key-only text.
    if clear {
        precheck(
            client,
            &effective_session_id,
            &object_id,
            "type-clear",
            selector_or_ref,
        )
        .await?;
    } else if !keys_only {
        precheck(
            client,
            &effective_session_id,
            &object_id,
            "type",
            selector_or_ref,
        )
        .await?;
    }
    let before = probe_object(client, &effective_session_id, &object_id)
        .await
        .unwrap_or_else(|_| FormProbe::unknown());

    focus_target(
        client,
        &effective_session_id,
        &object_id,
        selector_or_ref,
        if clear {
            "cleared and typed into"
        } else {
            "typed into"
        },
        clear || !keys_only,
    )
    .await?;
    if clear {
        clear_control(
            client,
            &effective_session_id,
            &object_id,
            selector_or_ref,
            "cleared and typed into",
        )
        .await?;
    }

    // A field may select its content on focus; that content is replaced by
    // the typed text, so it does not count towards the length held afterwards.
    let before = FormProbe {
        selected: probe_object(client, &effective_session_id, &object_id)
            .await
            .map(|p| p.selected)
            .unwrap_or(0),
        ..before
    };

    type_text_into_active_context(client, session_id, text, delay_ms).await?;

    let after = match read_after(probe_object(client, &effective_session_id, &object_id)).await {
        AfterRead::Value(after) => after,
        other => {
            return Ok(FormOutcome {
                warning: unverified_warning(&other),
            })
        }
    };
    Ok(FormOutcome {
        warning: type_read_warning(&before, &after, text, clear),
    })
}

/// What the read after `type` says about the page. Control characters are key
/// events and a password reads back empty, so neither is judged.
fn type_read_warning(
    before: &FormProbe,
    after: &FormProbe,
    text: &str,
    clear: bool,
) -> Option<String> {
    let plain = !text.is_empty() && !text.contains(['\n', '\r', '\t']);
    if !plain || after.password {
        return None;
    }
    if let Some(warning) = overflow_warning(before, after, text, clear) {
        return Some(warning);
    }
    if !clear && after.value.is_some() && after.value == before.value {
        return Some(format!(
            "effect_not_observed: {} did not change after typing; the page may apply the text later",
            after.describe()
        ));
    }
    type_warning(after, text)
}

/// Warns when the field has a maxlength that the old value (minus the part
/// selected for replacement) plus the typed text exceeds and the field ended
/// up full: the browser cut the typed text, even when what remains still
/// reads like it. Only lengths are reported.
fn overflow_warning(
    before: &FormProbe,
    after: &FormProbe,
    text: &str,
    clear: bool,
) -> Option<String> {
    let max = before.max_length;
    let held = after.value.as_ref()?.encode_utf16().count() as i64;
    let prior = if clear {
        0
    } else {
        (before.value.as_ref()?.encode_utf16().count() as i64 - before.selected).max(0)
    };
    let typed = text.encode_utf16().count() as i64;
    (max >= 0 && prior + typed > max && held >= max).then(|| {
        format!(
            "value_truncated: {} accepts at most {max} characters (maxlength) but {} would be held ({prior} existing, {typed} typed)",
            after.describe(),
            prior + typed
        )
    })
}

/// Warns when the typed text is not in the field afterwards, as a maxlength
/// cut or an input mask does. Where the caret was and what else the page adds
/// (a selected value replaced, an autocompleted suffix) are the page's; email
/// inputs trim what is typed. Only lengths are reported, never values.
fn type_warning(after: &FormProbe, text: &str) -> Option<String> {
    let held = after.value.as_ref()?;
    let squash = |value: &str| {
        let value = after.comparable(value);
        if after.content_editable {
            value.split_whitespace().collect::<String>()
        } else if after.input_type == "email" {
            value.trim().to_string()
        } else {
            value
        }
    };
    if squash(held).contains(&squash(text)) {
        return None;
    }
    Some(format!(
        "value_mismatch: {} does not hold the typed text (typed length {}, field length {}); the page may limit or rewrite the text (maxlength, input mask)",
        after.describe(),
        text.encode_utf16().count(),
        held.encode_utf16().count()
    ))
}

pub async fn type_text_into_active_context(
    client: &CdpClient,
    session_id: &str,
    text: &str,
    delay_ms: Option<u64>,
) -> Result<(), String> {
    let delay = delay_ms.unwrap_or(0);

    for ch in text.chars() {
        if matches!(ch, '\n' | '\r' | '\t') {
            let (key, code, key_code) = char_to_key_info(ch);
            let text_str = key_text(&key);
            client
                .send_command_typed::<_, Value>(
                    "Input.dispatchKeyEvent",
                    &DispatchKeyEventParams {
                        event_type: "keyDown".to_string(),
                        key: Some(key.clone()),
                        code: Some(code.clone()),
                        text: text_str.clone(),
                        unmodified_text: text_str,
                        windows_virtual_key_code: Some(key_code),
                        modifiers: None,
                        location: None,
                    },
                    Some(session_id),
                )
                .await?;

            client
                .send_command_typed::<_, Value>(
                    "Input.dispatchKeyEvent",
                    &DispatchKeyEventParams {
                        event_type: "keyUp".to_string(),
                        key: Some(key),
                        code: Some(code),
                        text: None,
                        unmodified_text: None,
                        windows_virtual_key_code: Some(key_code),
                        modifiers: None,
                        location: None,
                    },
                    Some(session_id),
                )
                .await?;
        } else {
            // VS Code/Electron webviews reject repeated dispatchKeyEvent calls
            // carrying printable `text`. Insert printable characters directly
            // and reserve key events for controls like Enter and Tab.
            client
                .send_command_typed::<_, Value>(
                    "Input.insertText",
                    &InsertTextParams {
                        text: ch.to_string(),
                    },
                    Some(session_id),
                )
                .await?;
        }

        if delay > 0 {
            tokio::time::sleep(tokio::time::Duration::from_millis(delay)).await;
        }
    }

    Ok(())
}

pub async fn press_key(client: &CdpClient, session_id: &str, key: &str) -> Result<(), String> {
    press_key_with_modifiers(client, session_id, key, None).await
}

/// Dispatch a keyDown+keyUp sequence for `key` with an optional CDP modifier bitmask.
///
/// Modifier values follow the CDP `Input.dispatchKeyEvent` spec:
/// 1 = Alt, 2 = Control, 4 = Meta (Cmd), 8 = Shift.
///
/// Callers that need a platform-appropriate modifier (e.g. Cmd on macOS,
/// Ctrl elsewhere) must choose the value themselves -- see `cfg!(target_os)`.
pub async fn press_key_with_modifiers(
    client: &CdpClient,
    session_id: &str,
    key: &str,
    modifiers: Option<i32>,
) -> Result<(), String> {
    press_key_with_chord_and_held(client, session_id, key, modifiers.unwrap_or(0), 0).await
}

/// Dispatch keyDown/keyUp for `key` with chord modifiers (emitting keyDown/keyUp
/// for chord modifiers not already held) combined with `held_modifiers`.
pub async fn press_key_with_chord_and_held(
    client: &CdpClient,
    session_id: &str,
    key: &str,
    chord_modifiers: i32,
    held_modifiers: i32,
) -> Result<(), String> {
    let chord_only = chord_modifiers & !held_modifiers;
    let modifier_keys = modifier_key_names(chord_only);
    let mut pressed_modifiers: Vec<&'static str> = Vec::with_capacity(modifier_keys.len());
    let mut active_modifiers = held_modifiers;

    for modifier in &modifier_keys {
        let bit = key_modifier_bit(modifier).expect("modifier names have a CDP bit");
        let next_modifiers = active_modifiers | bit;
        if let Err(error) = dispatch_single_key_event(
            client,
            session_id,
            modifier,
            "keyDown",
            nonzero_modifiers(next_modifiers),
        )
        .await
        {
            for pressed in pressed_modifiers.iter().rev() {
                active_modifiers &=
                    !key_modifier_bit(pressed).expect("modifier names have a CDP bit");
                let _ = dispatch_single_key_event(
                    client,
                    session_id,
                    pressed,
                    "keyUp",
                    nonzero_modifiers(active_modifiers),
                )
                .await;
            }
            return Err(error);
        }
        pressed_modifiers.push(*modifier);
        active_modifiers = next_modifiers;
    }

    let own_bit = key_modifier_bit(key).unwrap_or(0);
    let down_modifiers = nonzero_modifiers(active_modifiers | own_bit);
    let up_modifiers = nonzero_modifiers(active_modifiers & !(own_bit & !held_modifiers));

    let key_result = async {
        dispatch_single_key_event(client, session_id, key, "keyDown", down_modifiers).await?;
        dispatch_single_key_event(client, session_id, key, "keyUp", up_modifiers).await
    }
    .await;

    let mut release_error = None;
    for modifier in pressed_modifiers.iter().rev() {
        active_modifiers &= !key_modifier_bit(modifier).expect("modifier names have a CDP bit");
        if let Err(error) = dispatch_single_key_event(
            client,
            session_id,
            modifier,
            "keyUp",
            nonzero_modifiers(active_modifiers),
        )
        .await
        {
            release_error.get_or_insert(error);
        }
    }

    key_result?;
    release_error.map_or(Ok(()), Err)
}

/// Press `key` without releasing it and return the updated held modifier mask.
pub async fn key_down(
    client: &CdpClient,
    session_id: &str,
    key: &str,
    held_modifiers: i32,
) -> Result<i32, String> {
    let next_modifiers = held_modifiers | key_modifier_bit(key).unwrap_or(0);
    dispatch_single_key_event(
        client,
        session_id,
        key,
        "keyDown",
        nonzero_modifiers(next_modifiers),
    )
    .await?;
    Ok(next_modifiers)
}

/// Release `key` and return the updated held modifier mask.
pub async fn key_up(
    client: &CdpClient,
    session_id: &str,
    key: &str,
    held_modifiers: i32,
) -> Result<i32, String> {
    let next_modifiers = held_modifiers & !key_modifier_bit(key).unwrap_or(0);
    dispatch_single_key_event(
        client,
        session_id,
        key,
        "keyUp",
        nonzero_modifiers(next_modifiers),
    )
    .await?;
    Ok(next_modifiers)
}

async fn dispatch_single_key_event(
    client: &CdpClient,
    session_id: &str,
    key: &str,
    event_type: &str,
    modifiers: Option<i32>,
) -> Result<(), String> {
    client
        .send_command_typed::<_, Value>(
            "Input.dispatchKeyEvent",
            &single_key_event_params(key, event_type, modifiers),
            Some(session_id),
        )
        .await?;
    Ok(())
}

fn single_key_event_params(
    key: &str,
    event_type: &str,
    modifiers: Option<i32>,
) -> DispatchKeyEventParams {
    let (key_name, code, key_code, text, unmodified_text) = modified_key_info(key, modifiers);
    let is_down = event_type == "keyDown";
    let modifiers = if is_down {
        with_own_modifier_bit(&key_name, modifiers)
    } else {
        modifiers
    };
    DispatchKeyEventParams {
        event_type: event_type.to_string(),
        location: key_location(&code),
        key: Some(key_name),
        code: Some(code),
        text: if is_down { text } else { None },
        unmodified_text: if is_down { unmodified_text } else { None },
        windows_virtual_key_code: Some(key_code),
        modifiers,
    }
}

fn nonzero_modifiers(modifiers: i32) -> Option<i32> {
    (modifiers != 0).then_some(modifiers)
}

fn modifier_key_names(modifiers: i32) -> Vec<&'static str> {
    [(2, "Control"), (1, "Alt"), (8, "Shift"), (4, "Meta")]
        .into_iter()
        .filter_map(|(bit, key)| (modifiers & bit != 0).then_some(key))
        .collect()
}

pub async fn scroll(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: Option<&str>,
    delta_x: f64,
    delta_y: f64,
    iframe_sessions: &HashMap<String, String>,
) -> Result<(), String> {
    if let Some(sel) = selector_or_ref {
        let (object_id, effective_session_id) =
            resolve_element_object_id(client, session_id, ref_map, sel, iframe_sessions).await?;
        let js = "function(dx, dy) { this.scrollBy(dx, dy); }".to_string();
        client
            .send_command_typed::<_, Value>(
                "Runtime.callFunctionOn",
                &CallFunctionOnParams {
                    function_declaration: js,
                    object_id: Some(object_id),
                    arguments: Some(vec![
                        CallArgument {
                            value: Some(serde_json::json!(delta_x)),
                            object_id: None,
                        },
                        CallArgument {
                            value: Some(serde_json::json!(delta_y)),
                            object_id: None,
                        },
                    ]),
                    return_by_value: Some(true),
                    await_promise: Some(false),
                },
                Some(&effective_session_id),
            )
            .await?;
    } else {
        let js = format!("window.scrollBy({}, {})", delta_x, delta_y);
        client
            .send_command_typed::<_, Value>(
                "Runtime.evaluate",
                &EvaluateParams {
                    expression: js,
                    return_by_value: Some(true),
                    await_promise: Some(false),
                },
                Some(session_id),
            )
            .await?;
    }
    Ok(())
}

/// A refused `select`: the message is fixed text; the options the page offered
/// travel separately as error data.
pub struct SelectError {
    pub message: String,
    pub options: Option<Value>,
}

impl From<String> for SelectError {
    fn from(message: String) -> Self {
        Self {
            message,
            options: None,
        }
    }
}

pub async fn select_option(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    values: &[String],
    by: Option<&str>,
    iframe_sessions: &HashMap<String, String>,
) -> Result<(), SelectError> {
    let (object_id, effective_session_id) = resolve_element_object_id(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;

    // Matching nothing must be an error, not a silent success: an agent that
    // selects a misspelled option otherwise sees "Done", and only discovers
    // the page state is wrong after more commands. List what was available.
    //
    // Native <select> keeps its own path. For a div-based ARIA combobox/listbox
    // (Radix, Headless UI, ui-sdk) there is no `this.options`, so open the widget
    // when it is not already expanded, wait for its role=option items, and click
    // the ones whose accessible name or value matches.
    let js = r#"async function(vals, by) {
            const normalize = (value) => String(value ?? '')
                .replace(/[\u200B\u200C\u200D\u2060\uFEFF]/g, '')
                .replace(/\s+/g, ' ')
                .trim();
            if (!(this instanceof HTMLSelectElement)) {
                const role = this.getAttribute('role');
                if (role !== 'combobox' && role !== 'listbox') {
                    return { kind: 'not_select' };
                }
                const realClick = (el) => {
                    const r = el.getBoundingClientRect();
                    const o = { bubbles: true, cancelable: true, composed: true,
                                clientX: r.left + r.width / 2, clientY: r.top + r.height / 2,
                                button: 0, pointerId: 1, isPrimary: true };
                    el.dispatchEvent(new PointerEvent('pointerdown', o));
                    el.dispatchEvent(new MouseEvent('mousedown', o));
                    el.dispatchEvent(new PointerEvent('pointerup', o));
                    el.dispatchEvent(new MouseEvent('mouseup', o));
                    el.dispatchEvent(new MouseEvent('click', o));
                };
                // Clicking an open combobox toggles it shut, so only open when collapsed.
                if (role === 'combobox' && this.getAttribute('aria-expanded') !== 'true') {
                    realClick(this);
                }
                const collect = () => {
                    let scope = role === 'listbox' ? this : null;
                    if (!scope) {
                        const controls = this.getAttribute('aria-controls');
                        if (controls) {
                            // Radix sets aria-controls before the popup mounts, so a
                            // missing target means "not rendered yet": return nothing
                            // and let the wait loop retry, instead of collecting another
                            // listbox's options from the document.
                            scope = document.getElementById(controls);
                            if (!scope) return [];
                        }
                    }
                    const nodes = Array.from((scope || document).querySelectorAll('[role=option]'));
                    return nodes.filter((o) => o.getClientRects().length > 0);
                };
                let options = collect();
                for (let i = 0; i < 40 && options.length === 0; i++) {
                    await new Promise((r) => setTimeout(r, 50));
                    options = collect();
                }
                if (options.length === 0) {
                    return { kind: 'no_visible_options' };
                }
                // Accessible name precedence: aria-labelledby, aria-label, text
                // content. Option rows often carry an indicator or a description
                // next to the label, so textContent alone misses their name.
                const nameOf = (o) => {
                    const labels = (o.getAttribute('aria-labelledby') || '').trim();
                    if (labels) {
                        const text = labels.split(/\s+/)
                            .map((id) => { const el = document.getElementById(id); return el ? el.textContent : ''; })
                            .join(' ');
                        if (normalize(text)) return normalize(text);
                    }
                    return normalize(o.getAttribute('aria-label')) || normalize(o.textContent);
                };
                const valueOf = (o) => normalize(o.getAttribute('data-value')) || normalize(o.getAttribute('value'));
                const wanted = new Set();
                for (const value of vals) {
                    const want = normalize(value);
                    const matches = options.filter((o) => (by !== 'value' && nameOf(o) === want) || (by !== 'label' && valueOf(o) && valueOf(o) === want));
                    if (matches.length === 0) {
                        return { kind: 'not_found_list', n: options.length, options: options.map((o) => ({ value: valueOf(o), label: nameOf(o) })) };
                    }
                    if (matches.length > 1) {
                        return { kind: 'ambiguous_list', n: matches.length, value: vals.indexOf(value) };
                    }
                    // A disabled option swallows the click, so selecting it would
                    // report success with an unchanged widget.
                    if (matches[0].getAttribute('aria-disabled') === 'true') {
                        return { kind: 'option_disabled' };
                    }
                    wanted.add(matches[0]);
                }
                if (wanted.size === 0) {
                    return { kind: 'not_found_list', n: options.length, options: options.map((o) => ({ value: valueOf(o), label: nameOf(o) })) };
                }
                // A click toggles selection in a multi-select listbox, so an option
                // that is already selected must be left alone.
                const multi = role === 'listbox' && this.getAttribute('aria-multiselectable') === 'true';
                for (const opt of wanted) {
                    if (multi && opt.getAttribute('aria-selected') === 'true') continue;
                    opt.scrollIntoView({ block: 'nearest' });
                    realClick(opt);
                }
                return { matched: wanted.size };
            }
            if (this.matches(':disabled')) {
                return { kind: 'select_disabled' };
            }
            if (vals.length > 1 && !this.multiple) {
                return { kind: 'not_multiple' };
            }
            const options = Array.from(this.options);
            const labelOf = (o) => o.label.trim();
            // Options sharing a value are one choice.
            const distinct = (found) => new Set(found.map((o) => o.value)).size;
            // A string that reaches exactly one option, by value or by exact
            // label, selects it. Reaching options with different values, by the
            // same kind of match or across kinds, is ambiguous; `by` restricts
            // matching to one attribute so every option stays reachable.
            const resolve = (value) => {
                const byLabel = by === 'value' ? [] : options.filter((o) => value === labelOf(o));
                const byValue = by === 'label' ? [] : options.filter((o) => value === o.value);
                const all = byLabel.concat(byValue.filter((o) => !byLabel.includes(o)));
                if (all.length) return distinct(all) === 1 ? { found: all } : { found: all, ambiguous: true };
                if (by === 'value') return { found: [] };
                const wanted = normalize(value);
                const byNorm = options.filter((o) => normalize(o.label) === wanted);
                if (byNorm.length) return distinct(byNorm) === 1 ? { found: byNorm } : { found: byNorm, ambiguous: true };
                return { found: [] };
            };
            const wanted = new Set();
            for (const value of vals) {
                const r = resolve(value);
                if (r.found.length === 0) {
                    return { kind: 'not_found_select', n: options.length, options: options.map((o) => ({ value: o.value, label: normalize(o.label) })) };
                }
                if (r.ambiguous) {
                    const seen = new Set();
                    const candidates = r.found.filter((o) => !seen.has(o.value) && seen.add(o.value));
                    return { kind: 'ambiguous_select', n: candidates.length, value: vals.indexOf(value) };
                }
                for (const opt of r.found) wanted.add(opt);
            }
            if (wanted.size === 0) {
                return { kind: 'not_found_select', n: options.length, options: options.map((o) => ({ value: o.value, label: normalize(o.label) })) };
            }
            for (const opt of options) opt.selected = wanted.has(opt);
            this.dispatchEvent(new Event('change', { bubbles: true }));
            return { matched: wanted.size };
        }"#
    .to_string();

    let result = client
        .send_command_typed::<_, Value>(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: js,
                object_id: Some(object_id),
                arguments: Some(vec![
                    CallArgument {
                        value: Some(serde_json::json!(values)),
                        object_id: None,
                    },
                    CallArgument {
                        value: Some(serde_json::json!(by)),
                        object_id: None,
                    },
                ]),
                return_by_value: Some(true),
                await_promise: Some(true),
            },
            Some(&effective_session_id),
        )
        .await?;

    // A thrown script error must surface instead of reporting success; the
    // text of the error belongs to the page and is not repeated.
    if result.get("exceptionDetails").is_some() {
        return Err("select failed: the page script raised an error"
            .to_string()
            .into());
    }

    let value = result.get("result").and_then(|r| r.get("value"));
    if let Some((code, message)) = value.and_then(|v| select_failure(v, values, by)) {
        let message = match code {
            Some(code) => form_error(code, message),
            None => message,
        };
        return Err(SelectError {
            message,
            options: value.and_then(available_options),
        });
    }

    Ok(())
}

/// The options a refused `select` found, as `[{value, label}]` for the error
/// `data`. The list is page content: it stays out of the message and only
/// string fields are read.
pub fn available_options(page: &Value) -> Option<Value> {
    let list = page.get("options")?.as_array()?;
    let text = |option: &Value, key: &str| {
        option
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    Some(Value::Array(
        list.iter()
            .map(|o| serde_json::json!({ "value": text(o, "value"), "label": text(o, "label") }))
            .collect(),
    ))
}

/// The error for a refused `select`, built here from the failure kind the page
/// reports. Counts are numbers and values are looked up in what the caller
/// passed; nothing else from the page is used.
pub fn select_failure(
    page: &Value,
    values: &[String],
    by: Option<&str>,
) -> Option<(Option<&'static str>, String)> {
    let kind = page.get("kind").and_then(Value::as_str)?;
    let count = page.get("n").and_then(Value::as_u64).unwrap_or(0);
    let asked = serde_json::to_string(values).unwrap_or_default();
    let named = page
        .get("value")
        .and_then(Value::as_u64)
        .and_then(|i| values.get(i as usize))
        .map(|v| serde_json::to_string(v).unwrap_or_default())
        .unwrap_or_else(|| asked.clone());
    let by_text = by.map(|b| format!(" by {b}")).unwrap_or_default();
    let by_advice =
        " Pass --by value to match option values or --by label to match visible labels.";
    Some(match kind {
        "not_select" => (
            Some("not_editable"),
            "select target is not a <select> or a role=combobox/listbox element".to_string(),
        ),
        "no_visible_options" => (
            None,
            "combobox opened but no visible role=option elements were found".to_string(),
        ),
        "not_found_list" => (
            Some("option_not_found"),
            format!("No option matched {asked}. The list has {count} options"),
        ),
        "not_found_select" => (
            Some("option_not_found"),
            format!("No option matched {asked}{by_text}. The select has {count} options"),
        ),
        "ambiguous_list" => (
            Some("ambiguous"),
            format!("Multiple options matched {named} ({count} options).{by_advice}"),
        ),
        "ambiguous_select" => {
            let advice = if by == Some("label") {
                " Several options share that label; pass --by value with the option value."
            } else {
                by_advice
            };
            (
                Some("ambiguous"),
                format!(
                    "Multiple options matched {named} ({count} options with different values).{advice}"
                ),
            )
        }
        "option_disabled" => (
            Some("disabled"),
            "The matched option is disabled".to_string(),
        ),
        "select_disabled" => (
            Some("disabled"),
            "select target <select> is disabled".to_string(),
        ),
        "not_multiple" => (
            Some("not_multiple"),
            format!(
                "{} values requested but <select> does not allow multiple selection",
                values.len()
            ),
        ),
        // Multiselect kinds.
        "multi_not_select" => (
            Some("not_editable"),
            "Element is not a <select>".to_string(),
        ),
        "multi_disabled" => (
            Some("disabled"),
            "Element <select> is disabled and cannot be changed".to_string(),
        ),
        "multi_not_found" => {
            let missing: Vec<&String> = page
                .get("missing")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|i| i.as_u64().and_then(|i| values.get(i as usize)))
                        .collect()
                })
                .unwrap_or_default();
            (
                Some("option_not_found"),
                format!(
                    "No option matched {}. {count} options available",
                    serde_json::to_string(&missing).unwrap_or_default()
                ),
            )
        }
        _ => return None,
    })
}

/// What a check or uncheck did. `verified` is false when the page navigated
/// before the toggle could be read back; `warning` says the state did not
/// change as requested or could not be inspected.
#[derive(Debug)]
pub struct ToggleOutcome {
    pub position: Option<(f64, f64)>,
    pub verified: bool,
    pub warning: Option<String>,
}

impl Default for ToggleOutcome {
    fn default() -> Self {
        Self {
            position: None,
            verified: true,
            warning: None,
        }
    }
}

pub async fn check(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<ToggleOutcome, String> {
    Box::pin(toggle(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
        true,
    ))
    .await
}

pub async fn uncheck(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<ToggleOutcome, String> {
    Box::pin(toggle(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
        false,
    ))
    .await
}

async fn toggle(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
    want: bool,
) -> Result<ToggleOutcome, String> {
    let verb = if want { "checked" } else { "unchecked" };
    let Ok(probe) = probe_selector(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await
    else {
        return toggle_unprobed(
            client,
            session_id,
            ref_map,
            selector_or_ref,
            iframe_sessions,
            want,
        )
        .await;
    };
    if !probe.checkable {
        return Err(form_error(
            "not_checkable",
            format!(
                "Element {} is not a checkbox, radio or element with a checkbox-like role, so it cannot be {verb}",
                probe.describe()
            ),
        ));
    }
    if probe.checked.unwrap_or(false) == want {
        return Ok(ToggleOutcome::default());
    }
    if probe.disabled || probe.box_disabled {
        return Err(form_error(
            "disabled",
            format!(
                "Element {} is disabled and cannot be {verb}",
                probe.describe()
            ),
        ));
    }
    if !want && probe.radio {
        return Err(form_error(
            "not_checkable",
            "A checked radio cannot be unchecked by clicking; select another radio in its group instead",
        ));
    }

    let result = click_element(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        ClickSpec::default(),
        iframe_sessions,
        false,
    )
    .await?;
    let mut outcome = ToggleOutcome {
        position: Some(result.position),
        ..ToggleOutcome::default()
    };

    // A native input whose coordinate click missed (hidden input, overlay) is
    // retried with a DOM click. A custom control is not: a second click would
    // undo a first one the page applies late. The click has already happened,
    // so a read that fails afterwards ends the verification without failing
    // the action.
    let mut after = read_after(probe_selector(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    ))
    .await;
    if let AfterRead::Value(probe) = &after {
        if probe.checked.unwrap_or(false) != want && probe.native {
            let retried = read_after(js_click_checkbox(
                client,
                session_id,
                ref_map,
                selector_or_ref,
                iframe_sessions,
            ))
            .await;
            after = match retried {
                AfterRead::Value(()) => {
                    read_after(probe_selector(
                        client,
                        session_id,
                        ref_map,
                        selector_or_ref,
                        iframe_sessions,
                    ))
                    .await
                }
                AfterRead::Gone => AfterRead::Gone,
                AfterRead::Unreadable => AfterRead::Unreadable,
            };
        }
    }
    match after {
        AfterRead::Value(probe) => {
            if probe.checked.unwrap_or(false) != want {
                outcome.warning = Some(format!(
                    "effect_not_observed: {} is still {} after clicking; the page may apply the change later",
                    probe.describe(),
                    if want { "unchecked" } else { "checked" }
                ));
            }
        }
        AfterRead::Gone => outcome.verified = false,
        AfterRead::Unreadable => outcome.warning = unverified_warning(&AfterRead::<()>::Unreadable),
    }
    Ok(outcome)
}

/// Toggles without the preconditions when the element cannot be inspected, as
/// before: click when the state differs, retry once with a DOM click.
async fn toggle_unprobed(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
    want: bool,
) -> Result<ToggleOutcome, String> {
    let state = || {
        super::element::is_element_checked(
            client,
            session_id,
            ref_map,
            selector_or_ref,
            iframe_sessions,
        )
    };
    if state().await? == want {
        return Ok(ToggleOutcome::default());
    }
    let result = click_element(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        ClickSpec::default(),
        iframe_sessions,
        false,
    )
    .await?;
    let mut outcome = ToggleOutcome {
        position: Some(result.position),
        ..ToggleOutcome::default()
    };
    let mut now = read_after(state()).await;
    if matches!(now, AfterRead::Value(current) if current != want) {
        let retried = read_after(js_click_checkbox(
            client,
            session_id,
            ref_map,
            selector_or_ref,
            iframe_sessions,
        ))
        .await;
        now = match retried {
            AfterRead::Value(()) => read_after(state()).await,
            AfterRead::Gone => AfterRead::Gone,
            AfterRead::Unreadable => AfterRead::Unreadable,
        };
    }
    match now {
        AfterRead::Value(current) if current == want => {}
        AfterRead::Value(_) => {
            outcome.warning = Some("effect_not_observed: the state did not change after clicking; the page may apply the change later".to_string())
        }
        AfterRead::Gone => outcome.verified = false,
        AfterRead::Unreadable => {
            outcome.warning = unverified_warning(&AfterRead::<()>::Unreadable)
        }
    }
    Ok(outcome)
}

/// Fallback for when the coordinate-based CDP click did not toggle the
/// checkbox/radio state: dispatch the click through the DOM, following the
/// label to its control, as Playwright does.
async fn js_click_checkbox(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<(), String> {
    let (object_id, effective_session_id) = resolve_element_object_id(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;

    let js = r#"function() {
            var el = this;
            var tag = el.tagName && el.tagName.toUpperCase();
            // 1. Native input — click it directly
            if (tag === 'INPUT' && (el.type === 'checkbox' || el.type === 'radio')) {
                el.click();
                return;
            }
            // 2. Follow label → control association
            var label = tag === 'LABEL' ? el : (el.closest && el.closest('label'));
            if (label && label.tagName && label.tagName.toUpperCase() === 'LABEL' && label.control) {
                label.control.click();
                return;
            }
            // 3. Nested native input
            var input = el.querySelector && el.querySelector('input[type="checkbox"], input[type="radio"]');
            if (input) {
                input.click();
                return;
            }
            // 4. ARIA role control — click the element itself
            el.click();
        }"#;

    client
        .send_command_typed::<_, Value>(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: js.to_string(),
                object_id: Some(object_id),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(&effective_session_id),
        )
        .await?;

    Ok(())
}

pub async fn focus(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<(), String> {
    let (object_id, effective_session_id) = resolve_element_object_id(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;

    client
        .send_command_typed::<_, Value>(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: "function() { this.focus(); }".to_string(),
                object_id: Some(object_id),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(&effective_session_id),
        )
        .await?;

    Ok(())
}

pub async fn clear(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<(), String> {
    let (object_id, effective_session_id) = resolve_element_object_id(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;

    client
        .send_command_typed::<_, Value>(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: r#"function() {
                    this.focus();
                    this.value = '';
                    this.dispatchEvent(new Event('input', { bubbles: true }));
                    this.dispatchEvent(new Event('change', { bubbles: true }));
                }"#
                .to_string(),
                object_id: Some(object_id),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(&effective_session_id),
        )
        .await?;

    Ok(())
}

pub async fn select_all(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<(), String> {
    let (object_id, effective_session_id) = resolve_element_object_id(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;

    client
        .send_command_typed::<_, Value>(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: r#"function() {
                    this.focus();
                    if (typeof this.select === 'function') {
                        this.select();
                    } else {
                        const range = document.createRange();
                        range.selectNodeContents(this);
                        const sel = window.getSelection();
                        sel.removeAllRanges();
                        sel.addRange(range);
                    }
                }"#
                .to_string(),
                object_id: Some(object_id),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(&effective_session_id),
        )
        .await?;

    Ok(())
}

pub async fn scroll_into_view(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<(), String> {
    let (object_id, effective_session_id) = resolve_element_object_id(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;

    client
        .send_command_typed::<_, Value>(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration:
                    "function() { this.scrollIntoView({ block: 'center', inline: 'center' }); }"
                        .to_string(),
                object_id: Some(object_id),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(&effective_session_id),
        )
        .await?;

    Ok(())
}

pub async fn dispatch_event(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    event_type: &str,
    event_init: Option<&Value>,
    iframe_sessions: &HashMap<String, String>,
) -> Result<(), String> {
    let (object_id, effective_session_id) = resolve_element_object_id(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;

    let init_json = event_init
        .map(|v| serde_json::to_string(v).unwrap_or("{}".to_string()))
        .unwrap_or_else(|| "{ bubbles: true }".to_string());

    let js = format!(
        "function() {{ this.dispatchEvent(new Event({}, {})); }}",
        serde_json::to_string(event_type).unwrap_or_default(),
        init_json
    );

    client
        .send_command_typed::<_, Value>(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: js,
                object_id: Some(object_id),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(&effective_session_id),
        )
        .await?;

    Ok(())
}

pub async fn highlight(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<(), String> {
    let (object_id, effective_session_id) = resolve_element_object_id(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;

    client
        .send_command_typed::<_, Value>(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: r#"function() {
                    this.style.outline = '2px solid red';
                    this.style.outlineOffset = '2px';
                    const el = this;
                    setTimeout(() => {
                        el.style.outline = '';
                        el.style.outlineOffset = '';
                    }, 3000);
                }"#
                .to_string(),
                object_id: Some(object_id),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(&effective_session_id),
        )
        .await?;

    Ok(())
}

pub async fn tap_touch(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<(), String> {
    let (x, y, effective_session_id) = resolve_element_center(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
        false,
    )
    .await?;

    client
        .send_command(
            "Input.dispatchTouchEvent",
            Some(serde_json::json!({
                "type": "touchStart",
                "touchPoints": [{ "x": x, "y": y }],
            })),
            Some(&effective_session_id),
        )
        .await?;

    client
        .send_command(
            "Input.dispatchTouchEvent",
            Some(serde_json::json!({
                "type": "touchEnd",
                "touchPoints": [],
            })),
            Some(&effective_session_id),
        )
        .await?;

    Ok(())
}

/// Dispatches one mouse event and waits for the browser to ack it, but
/// returns Ok(true) if a JavaScript dialog opens first. A synchronous dialog
/// (confirm/prompt/alert in the event handler) blocks the renderer's main
/// thread, so the input ack cannot arrive until the dialog is resolved;
/// without this the command hangs until the client read timeout and the agent
/// never sees the pending-dialog warning.
async fn dispatch_mouse_or_dialog(
    client: &CdpClient,
    session_id: &str,
    accept_sessions: &[&str],
    params: &DispatchMouseEventParams,
) -> Result<bool, String> {
    use tokio::sync::broadcast::error::RecvError;

    // Subscribe before sending so the dialog event cannot slip past us.
    let mut events = client.subscribe();
    let send =
        client.send_command_typed::<_, Value>("Input.dispatchMouseEvent", params, Some(session_id));
    tokio::pin!(send);
    loop {
        tokio::select! {
            res = &mut send => {
                res?;
                return Ok(false);
            }
            event = events.recv() => {
                match event {
                    Ok(e) if e.method == "Page.javascriptDialogOpening" => {
                        // Only a dialog on this click's frame/page session
                        // aborts it; a background-tab dialog must not. A
                        // session-less event has no flat session and is
                        // treated as the top-level page (i.e. ours).
                        let ours = match e.session_id.as_deref() {
                            Some(sid) => accept_sessions.contains(&sid),
                            None => true,
                        };
                        if ours {
                            return Ok(true);
                        }
                        continue;
                    }
                    Ok(_) => continue,
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => {
                        (&mut send).await?;
                        return Ok(false);
                    }
                }
            }
        }
    }
}

/// One step of a click: the pointer moves to the target, then each click presses and releases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClickPhase {
    Move,
    Press(u8),
    Release(u8),
}

impl ClickPhase {
    /// The phase that follows this one, or None when the last click is released.
    fn next(self, spec: &ClickSpec) -> Option<Self> {
        match self {
            Self::Move => Some(Self::Press(1)),
            Self::Press(n) => Some(Self::Release(n)),
            Self::Release(n) if n < spec.count => Some(Self::Press(n + 1)),
            Self::Release(_) => None,
        }
    }

    /// The CDP mouse event this phase dispatches.
    fn event(self, spec: &ClickSpec, x: f64, y: f64) -> DispatchMouseEventParams {
        let modifiers = spec.modifiers.cdp();
        match self {
            Self::Move => DispatchMouseEventParams {
                event_type: "mouseMoved".to_string(),
                x,
                y,
                button: None,
                buttons: None,
                click_count: None,
                delta_x: None,
                delta_y: None,
                modifiers,
            },
            Self::Press(n) => DispatchMouseEventParams {
                event_type: "mousePressed".to_string(),
                x,
                y,
                button: Some(spec.button.name().to_string()),
                buttons: Some(spec.button.mask()),
                click_count: Some(i32::from(n)),
                delta_x: None,
                delta_y: None,
                modifiers,
            },
            Self::Release(n) => DispatchMouseEventParams {
                event_type: "mouseReleased".to_string(),
                x,
                y,
                button: Some(spec.button.name().to_string()),
                buttons: Some(0),
                click_count: Some(i32::from(n)),
                delta_x: None,
                delta_y: None,
                modifiers,
            },
        }
    }
}

async fn dispatch_click(
    client: &CdpClient,
    session_id: &str,
    accept_sessions: &[&str],
    x: f64,
    y: f64,
    spec: ClickSpec,
) -> Result<ClickResult, String> {
    let mut phase = Some(ClickPhase::Move);
    let mut button_pressed = false;
    while let Some(current) = phase {
        if matches!(current, ClickPhase::Press(_)) {
            button_pressed = true;
        }
        if dispatch_mouse_or_dialog(
            client,
            session_id,
            accept_sessions,
            &current.event(&spec, x, y),
        )
        .await?
        {
            return Ok(ClickResult {
                position: (x, y),
                dialog_opened: true,
                pending_release: match current {
                    ClickPhase::Press(n) => Some(PendingRelease {
                        session_id: session_id.to_string(),
                        x,
                        y,
                        button: spec.button,
                        click_count: n,
                        modifiers: spec.modifiers,
                    }),
                    _ => None,
                },
                x,
                y,
                button_pressed,
            });
        }
        phase = current.next(&spec);
    }
    Ok(ClickResult {
        position: (x, y),
        dialog_opened: false,
        pending_release: None,
        x,
        y,
        button_pressed: true,
    })
}

/// Best-effort mouseReleased to clear a button left logically down when a
/// dialog opened mid-click. Called after the dialog is resolved.
pub async fn dispatch_pending_release(
    client: &CdpClient,
    release: &PendingRelease,
) -> Result<(), String> {
    client
        .send_command_typed::<_, Value>(
            "Input.dispatchMouseEvent",
            &DispatchMouseEventParams {
                event_type: "mouseReleased".to_string(),
                x: release.x,
                y: release.y,
                button: Some(release.button.name().to_string()),
                buttons: Some(0),
                click_count: Some(i32::from(release.click_count)),
                delta_x: None,
                delta_y: None,
                modifiers: release.modifiers.cdp(),
            },
            Some(&release.session_id),
        )
        .await?;
    Ok(())
}

fn char_to_key_info(ch: char) -> (String, String, i32) {
    match ch {
        '\n' | '\r' => ("Enter".to_string(), "Enter".to_string(), 13),
        '\t' => ("Tab".to_string(), "Tab".to_string(), 9),
        ' ' => (" ".to_string(), "Space".to_string(), 32),
        _ => {
            let key = ch.to_string();
            if ch.is_ascii_alphabetic() {
                // For letters the Windows VK code equals the uppercase ASCII value.
                let upper = ch.to_ascii_uppercase();
                let code = format!("Key{}", upper);
                let key_code = upper as i32;
                (key, code, key_code)
            } else if ch.is_ascii_digit() {
                let code = format!("Digit{}", ch);
                let key_code = ch as i32;
                (key, code, key_code)
            } else {
                let (code, key_code) = punctuation_key_info(ch);
                (key, code.to_string(), key_code)
            }
        }
    }
}

/// Return the DOM `KeyboardEvent.code` value and Windows virtual-key code for
/// a punctuation / symbol character assuming a US keyboard layout.
///
/// The Windows virtual-key codes (VK_OEM_*) differ from ASCII values for
/// punctuation.  Using the raw ASCII code would misidentify characters – e.g.
/// '.' (ASCII 46) collides with VK_DELETE (0x2E = 46), causing the period to
/// be swallowed.
fn punctuation_key_info(ch: char) -> (&'static str, i32) {
    match ch {
        // VK_OEM_1 (0xBA = 186) — ";:" key on US layout
        ';' | ':' => ("Semicolon", 186),
        // VK_OEM_PLUS (0xBB = 187) — "=+" key
        '=' | '+' => ("Equal", 187),
        // VK_OEM_COMMA (0xBC = 188) — ",<" key
        ',' | '<' => ("Comma", 188),
        // VK_OEM_MINUS (0xBD = 189) — "-_" key
        '-' | '_' => ("Minus", 189),
        // VK_OEM_PERIOD (0xBE = 190) — ".>" key
        '.' | '>' => ("Period", 190),
        // VK_OEM_2 (0xBF = 191) — "/?" key
        '/' | '?' => ("Slash", 191),
        // VK_OEM_3 (0xC0 = 192) — "`~" key
        '`' | '~' => ("Backquote", 192),
        // VK_OEM_4 (0xDB = 219) — "[{" key
        '[' | '{' => ("BracketLeft", 219),
        // VK_OEM_5 (0xDC = 220) — "\\|" key
        '\\' | '|' => ("Backslash", 220),
        // VK_OEM_6 (0xDD = 221) — "]}" key
        ']' | '}' => ("BracketRight", 221),
        // VK_OEM_7 (0xDE = 222) — "'\""" key
        '\'' | '"' => ("Quote", 222),
        _ => ("", 0),
    }
}

/// Return the `text` value that CDP `Input.dispatchKeyEvent` needs on the
/// `keyDown` event so that Chrome performs the default action for the key.
/// For example Enter needs `"\r"` to actually submit a form, and Tab needs
/// `"\t"` to move focus.  Non-printable / navigation keys return `None`.
fn key_text(key_name: &str) -> Option<String> {
    match key_name {
        "Enter" => Some("\r".to_string()),
        "Tab" => Some("\t".to_string()),
        " " => Some(" ".to_string()),
        _ => {
            // Single printable characters carry themselves as text.
            if key_name.len() == 1 {
                Some(key_name.to_string())
            } else {
                None
            }
        }
    }
}

fn modified_key_info(
    key: &str,
    modifiers: Option<i32>,
) -> (String, String, i32, Option<String>, Option<String>) {
    let (unmodified_key, code, key_code) = named_key_info(key);
    let mask = modifiers.unwrap_or(0);
    let key_name = if mask & 8 != 0 {
        shifted_char(&unmodified_key)
            .map(|ch| ch.to_string())
            .unwrap_or_else(|| unmodified_key.clone())
    } else {
        unmodified_key.clone()
    };
    let (text, unmodified_text) = if mask & (1 | 2 | 4) != 0 {
        (None, None)
    } else {
        (key_text(&key_name), key_text(&unmodified_key))
    };
    (key_name, code, key_code, text, unmodified_text)
}

pub(crate) fn shifted_char(key_name: &str) -> Option<char> {
    let mut chars = key_name.chars();
    let ch = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    if ch.is_ascii_lowercase() {
        return Some(ch.to_ascii_uppercase());
    }
    const PAIRS: &[(char, char)] = &[
        ('1', '!'),
        ('2', '@'),
        ('3', '#'),
        ('4', '$'),
        ('5', '%'),
        ('6', '^'),
        ('7', '&'),
        ('8', '*'),
        ('9', '('),
        ('0', ')'),
        ('`', '~'),
        ('-', '_'),
        ('=', '+'),
        ('[', '{'),
        (']', '}'),
        ('\\', '|'),
        (';', ':'),
        ('\'', '"'),
        (',', '<'),
        ('.', '>'),
        ('/', '?'),
    ];
    PAIRS
        .iter()
        .find(|(plain, _)| *plain == ch)
        .map(|(_, shifted)| *shifted)
}

fn named_key_info(key: &str) -> (String, String, i32) {
    match key.to_lowercase().as_str() {
        "enter" | "return" => ("Enter".to_string(), "Enter".to_string(), 13),
        "tab" => ("Tab".to_string(), "Tab".to_string(), 9),
        "escape" | "esc" => ("Escape".to_string(), "Escape".to_string(), 27),
        "backspace" => ("Backspace".to_string(), "Backspace".to_string(), 8),
        "delete" => ("Delete".to_string(), "Delete".to_string(), 46),
        "arrowup" | "up" => ("ArrowUp".to_string(), "ArrowUp".to_string(), 38),
        "arrowdown" | "down" => ("ArrowDown".to_string(), "ArrowDown".to_string(), 40),
        "arrowleft" | "left" => ("ArrowLeft".to_string(), "ArrowLeft".to_string(), 37),
        "arrowright" | "right" => ("ArrowRight".to_string(), "ArrowRight".to_string(), 39),
        "home" => ("Home".to_string(), "Home".to_string(), 36),
        "end" => ("End".to_string(), "End".to_string(), 35),
        "pageup" => ("PageUp".to_string(), "PageUp".to_string(), 33),
        "pagedown" => ("PageDown".to_string(), "PageDown".to_string(), 34),
        "space" | " " => (" ".to_string(), "Space".to_string(), 32),
        "shift" | "shiftleft" => ("Shift".to_string(), "ShiftLeft".to_string(), 16),
        "shiftright" => ("Shift".to_string(), "ShiftRight".to_string(), 16),
        "control" | "ctrl" | "controlleft" => {
            ("Control".to_string(), "ControlLeft".to_string(), 17)
        }
        "controlright" => ("Control".to_string(), "ControlRight".to_string(), 17),
        "alt" | "altleft" => ("Alt".to_string(), "AltLeft".to_string(), 18),
        "altright" => ("Alt".to_string(), "AltRight".to_string(), 18),
        "meta" | "cmd" | "command" | "metaleft" => ("Meta".to_string(), "MetaLeft".to_string(), 91),
        "metaright" => ("Meta".to_string(), "MetaRight".to_string(), 92),
        lower => {
            let code_char = lower
                .strip_prefix("key")
                .filter(|rest| rest.len() == 1 && rest.as_bytes()[0].is_ascii_lowercase())
                .or_else(|| {
                    lower
                        .strip_prefix("digit")
                        .filter(|rest| rest.len() == 1 && rest.as_bytes()[0].is_ascii_digit())
                });
            if let Some(rest) = code_char {
                char_to_key_info(rest.chars().next().unwrap())
            } else if key.len() == 1 {
                let ch = key.chars().next().unwrap();
                char_to_key_info(ch)
            } else {
                (key.to_string(), key.to_string(), 0)
            }
        }
    }
}

fn key_location(code: &str) -> Option<i32> {
    match code {
        "ShiftLeft" | "ControlLeft" | "AltLeft" | "MetaLeft" => Some(1),
        "ShiftRight" | "ControlRight" | "AltRight" | "MetaRight" => Some(2),
        _ => None,
    }
}

pub(crate) fn key_modifier_bit(key: &str) -> Option<i32> {
    match key.to_ascii_lowercase().as_str() {
        "alt" | "altleft" | "altright" => Some(1),
        "control" | "ctrl" | "controlleft" | "controlright" => Some(2),
        "meta" | "cmd" | "command" | "metaleft" | "metaright" => Some(4),
        "shift" | "shiftleft" | "shiftright" => Some(8),
        _ => None,
    }
}

fn with_own_modifier_bit(key_name: &str, modifiers: Option<i32>) -> Option<i32> {
    let Some(bit) = key_modifier_bit(key_name) else {
        return modifiers;
    };
    Some(modifiers.unwrap_or(0) | bit)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify that `char_to_key_info` returns the correct (key, code,
    /// windowsVirtualKeyCode) triple for every character in Playwright's
    /// USKeyboardLayout.  The expected values below are taken verbatim from
    /// playwright-core/lib/server/usKeyboardLayout.js so that any drift from
    /// Playwright's behaviour is caught immediately.
    #[test]
    fn test_char_to_key_info_matches_playwright_layout() {
        // (character, expected_code, expected_vk_code)
        let cases: &[(char, &str, i32)] = &[
            // Letters – VK code must equal the uppercase ASCII value.
            ('a', "KeyA", 65),
            ('z', "KeyZ", 90),
            ('A', "KeyA", 65),
            // Digits
            ('0', "Digit0", 48),
            ('9', "Digit9", 57),
            // Punctuation – these are the values from Playwright's layout.
            // The bug that prompted this test sent '.' as VK 46 (= VK_DELETE).
            ('.', "Period", 190),
            (',', "Comma", 188),
            ('/', "Slash", 191),
            (';', "Semicolon", 186),
            ('\'', "Quote", 222),
            ('[', "BracketLeft", 219),
            (']', "BracketRight", 221),
            ('\\', "Backslash", 220),
            ('`', "Backquote", 192),
            ('-', "Minus", 189),
            ('=', "Equal", 187),
            // Shifted variants produced by the same physical keys.
            ('>', "Period", 190),
            ('<', "Comma", 188),
            ('?', "Slash", 191),
            (':', "Semicolon", 186),
            ('"', "Quote", 222),
            ('{', "BracketLeft", 219),
            ('}', "BracketRight", 221),
            ('|', "Backslash", 220),
            ('~', "Backquote", 192),
            ('_', "Minus", 189),
            ('+', "Equal", 187),
            // Whitespace / control
            (' ', "Space", 32),
            ('\n', "Enter", 13),
            ('\t', "Tab", 9),
        ];

        for &(ch, expected_code, expected_vk) in cases {
            let (key, code, vk) = char_to_key_info(ch);
            assert_eq!(
                code, expected_code,
                "char {:?}: expected code {:?}, got {:?}",
                ch, expected_code, code
            );
            assert_eq!(
                vk, expected_vk,
                "char {:?}: expected VK {}, got {} (ASCII would be {})",
                ch, expected_vk, vk, ch as i32
            );
            // key should be the character itself (except control chars).
            if !ch.is_control() {
                assert_eq!(key, ch.to_string(), "char {:?}: key mismatch", ch);
            }
        }
    }

    /// Regression test: period must NEVER map to VK 46 (VK_DELETE).
    #[test]
    fn test_period_is_not_vk_delete() {
        let (_, _, vk) = char_to_key_info('.');
        assert_ne!(
            vk, 46,
            "Period must not use VK code 46 (VK_DELETE); expected 190 (VK_OEM_PERIOD)"
        );
        assert_eq!(vk, 190);
    }

    /// Characters outside the US keyboard layout should return (key, "", 0)
    /// so that `type_text` falls back to `Input.insertText`.
    #[test]
    fn test_unmapped_chars_return_zero_keycode() {
        for ch in ['@', '#', '$', '%', '^', '&', '*', '(', ')', '€', '£', '你'] {
            let (key, code, vk) = char_to_key_info(ch);
            assert_eq!(
                code, "",
                "char {:?}: unmapped char should have empty code, got {:?}",
                ch, code
            );
            assert_eq!(
                vk, 0,
                "char {:?}: unmapped char should have VK 0, got {}",
                ch, vk
            );
            assert_eq!(key, ch.to_string());
        }
    }

    #[test]
    fn click_phase_walks_move_press_release_for_each_click() {
        let spec = ClickSpec {
            button: MouseButton::Right,
            count: 2,
            modifiers: Modifiers::parse_list("ctrl+shift").unwrap(),
        };
        let mut phases = Vec::new();
        let mut phase = Some(ClickPhase::Move);
        while let Some(current) = phase {
            phases.push(current);
            phase = current.next(&spec);
        }
        assert_eq!(
            phases,
            [
                ClickPhase::Move,
                ClickPhase::Press(1),
                ClickPhase::Release(1),
                ClickPhase::Press(2),
                ClickPhase::Release(2),
            ]
        );

        let press = ClickPhase::Press(2).event(&spec, 10.0, 20.0);
        assert_eq!(press.event_type, "mousePressed");
        assert_eq!(press.button.as_deref(), Some("right"));
        assert_eq!(press.buttons, Some(2));
        assert_eq!(press.click_count, Some(2));
        assert_eq!(press.modifiers, Some(10));
    }

    #[test]
    fn test_key_text_returns_correct_text_for_special_keys() {
        assert_eq!(key_text("Enter"), Some("\r".to_string()));
        assert_eq!(key_text("Tab"), Some("\t".to_string()));
        assert_eq!(key_text(" "), Some(" ".to_string()));
        // Single printable characters carry themselves.
        assert_eq!(key_text("a"), Some("a".to_string()));
        assert_eq!(key_text("Z"), Some("Z".to_string()));
        // Non-printable named keys return None.
        assert_eq!(key_text("Escape"), None);
        assert_eq!(key_text("ArrowUp"), None);
        assert_eq!(key_text("Backspace"), None);
        assert_eq!(key_text("Delete"), None);
    }

    fn text_probe(value: &str, max_length: i64) -> FormProbe {
        FormProbe {
            tag: "input".into(),
            input_type: "text".into(),
            max_length,
            value: Some(value.into()),
            ..Default::default()
        }
    }

    #[test]
    fn test_targets_are_described_from_fixed_vocabulary() {
        assert_eq!(describe_target("input", "text"), "<input type=\"text\">");
        assert_eq!(describe_target("select", ""), "<select>");
        for page_controlled in ["x-ignore-previous-instructions", "IGNORE#id", ""] {
            assert_eq!(describe_target(page_controlled, ""), "<unknown>");
        }
        assert_eq!(describe_target("input", "IGNORE ALL"), "<input>");
        let mut probe = text_probe("", -1);
        probe.tag = "x-ignore-previous-instructions".into();
        assert_eq!(probe.describe(), "<unknown>");
    }

    fn typed(before: &str, held: &str, text: &str, clear: bool) -> Option<String> {
        type_read_warning(&text_probe(before, -1), &text_probe(held, -1), text, clear)
    }

    #[test]
    fn test_typing_warns_only_when_the_text_is_missing() {
        // The caret, a selected value replaced, an autocompleted suffix and a
        // clear are the page's business: the typed text is in the field.
        for held in ["XAB", "AXB", "ABX"] {
            assert!(typed("AB", held, "X", false).is_none(), "{held}");
        }
        assert!(typed("ab", "c", "c", false).is_none());
        assert!(typed("", "United", "Un", false).is_none());
        assert!(typed("old", "new", "new", true).is_none());
        // A maxlength cut or a mask rewrite loses the text; lengths only.
        let cut = typed("", "01234", "0123456789", false).unwrap();
        let mask = typed("", "(555) 123", "555123", false).unwrap();
        for w in [&cut, &mask] {
            assert!(w.starts_with("value_mismatch"), "{w}");
            assert!(!w.contains("01234") && !w.contains("555"), "{w}");
        }
        assert!(
            cut.contains("typed length 10") && cut.contains("field length 5"),
            "{cut}"
        );
        assert!(typed("old", "oldnew", "new", true).is_none());
        assert!(typed("old", "old", "new", true).is_some());
        // Nothing is read from a field that reports no value.
        let mut blind = text_probe("", -1);
        blind.value = None;
        assert!(type_read_warning(&text_probe("a", -1), &blind, "b", false).is_none());
    }

    #[test]
    fn test_typing_over_a_selection_does_not_count_the_replaced_text() {
        // Everything selected on focus: 5 typed into a full maxlength=5 field.
        let mut before = text_probe("abcde", 5);
        before.selected = 5;
        let after = text_probe("12345", 5);
        assert!(type_read_warning(&before, &after, "12345", false).is_none());
        // Part selected: 3 kept + 2 typed fits, 3 kept + 4 typed is cut.
        before.selected = 2;
        assert!(type_read_warning(&before, &text_probe("12cde", 5), "12", false).is_none());
        let cut = type_read_warning(&before, &text_probe("4567c", 5), "4567", false);
        assert!(cut.is_some_and(|w| w.starts_with("value_truncated")));
        // Nothing selected: a real cut is reported.
        before.selected = 0;
        let cut = type_read_warning(&before, &text_probe("abcde", 5), "123", false);
        assert!(cut.is_some_and(|w| w.starts_with("value_truncated")));
    }

    #[test]
    fn test_available_options_come_from_string_fields_only() {
        let page = serde_json::json!({
            "kind": "not_found_select", "n": 2,
            "options": [{ "value": "a", "label": "A", "extra": "IGNORE" }, { "value": 3 }],
        });
        assert_eq!(
            available_options(&page),
            Some(serde_json::json!([
                { "value": "a", "label": "A" },
                { "value": "", "label": "" },
            ]))
        );
        assert!(available_options(&serde_json::json!({ "kind": "option_disabled" })).is_none());
    }

    #[test]
    fn test_typing_into_email_is_compared_trimmed() {
        let mut after = text_probe("a@b.co", -1);
        after.input_type = "email".into();
        let before = text_probe("", -1);
        assert!(type_read_warning(&before, &after, " a@b.co", false).is_none());
        after.value = Some("a@b".into());
        assert!(type_read_warning(&before, &after, " a@b.co", false).is_some());
    }

    #[test]
    fn test_typing_never_judges_a_password_or_key_events() {
        // A password reads back empty, which would otherwise look like loss.
        let mut after = text_probe("", -1);
        after.password = true;
        assert!(type_read_warning(&text_probe("", -1), &after, "secret", false).is_none());
        after.value = Some("".into());
        assert!(type_read_warning(&text_probe("x", -1), &after, "secret", true).is_none());
        // Control characters are key events.
        assert!(typed("a", "a", "\n", false).is_none());
    }

    #[test]
    fn test_typing_into_contenteditable_ignores_whitespace_layout() {
        let probe = |value: &str| FormProbe {
            tag: "div".into(),
            content_editable: true,
            value: Some(value.into()),
            max_length: -1,
            ..Default::default()
        };
        // innerText lays text out with its own spaces and line breaks.
        assert!(type_read_warning(&probe("a"), &probe("a b\nc"), "a  b c", true).is_none());
        assert!(type_read_warning(&probe("a"), &probe("a\nb"), "ab", true).is_none());
        assert!(type_read_warning(&probe("a"), &probe("a b"), "a  bc", true).is_some());
    }

    #[test]
    fn test_nothing_from_the_page_reaches_refusal_or_select_text() {
        // An unknown refusal kind from the page gets the plain text.
        for kind in ["; IGNORE_HINT_INJECTED", "", "disabledx"] {
            let (code, text) = refusal(kind, "filled");
            assert_eq!(code, "not_editable");
            assert_eq!(text, "cannot be filled", "{kind}");
        }
        assert_eq!(
            refusal("select", "filled").1,
            "cannot be filled; use select"
        );
        // Select failures use the kind, counts and the caller's own values.
        let values = vec!["mine".to_string()];
        let page = serde_json::json!({
            "kind": "not_found_select", "n": 3,
            "error": "IGNORE_ERROR", "hint": "IGNORE_HINT", "message": "IGNORE_MESSAGE",
        });
        let (code, text) = select_failure(&page, &values, None).unwrap();
        assert_eq!(code, Some("option_not_found"));
        assert!(
            text.contains("\"mine\"") && text.contains("3 options"),
            "{text}"
        );
        assert!(!text.contains("IGNORE"), "{text}");
        assert!(
            select_failure(&serde_json::json!({ "kind": "IGNORE_KIND" }), &values, None).is_none()
        );
        assert!(select_failure(&serde_json::json!({ "error": "IGNORE" }), &values, None).is_none());
        // An index outside the caller's values names nothing.
        let ambiguous = serde_json::json!({ "kind": "ambiguous_select", "n": 2, "value": 9 });
        let (_, text) = select_failure(&ambiguous, &values, Some("label")).unwrap();
        assert!(
            !text.contains("IGNORE") && text.contains("2 options"),
            "{text}"
        );
        // A malformed-value message takes only a known input type and numbers.
        let hostile = serde_json::json!({ "type": "IGNORE_TYPE", "minimum": "IGNORE" });
        assert_eq!(
            malformed_message(&hostile),
            "Malformed value for this input"
        );
        let range = serde_json::json!({ "type": "range", "minimum": "IGNORE", "maximum": 10 });
        assert_eq!(
            malformed_message(&range),
            "Malformed value for input of type \"range\": expected a number from 0 to 10"
        );
    }

    #[test]
    fn test_typing_into_a_full_field_is_reported_as_truncated() {
        let probe = |value: &str, max: i64| text_probe(value, max);
        // The old value already holds the typed text, so the cut is invisible
        // to a presence check.
        let w = type_read_warning(
            &probe("hello world", 12),
            &probe("whello world", 12),
            "world",
            false,
        )
        .unwrap();
        assert!(
            w.starts_with("value_truncated") && !w.contains("hello"),
            "{w}"
        );
        // A page that selects the value on focus replaces it; the field is not full.
        assert!(type_read_warning(&probe("abc", 5), &probe("def", 5), "def", false).is_none());
        // No maxlength, no warning from this check.
        assert!(type_read_warning(&probe("abc", -1), &probe("abcd", -1), "d", false).is_none());
    }

    #[test]
    fn test_form_error_code_matches_only_known_prefixes() {
        assert_eq!(form_error_code("read_only: x"), Some("read_only"));
        assert_eq!(
            form_error_code("ambiguous: Multiple options"),
            Some("ambiguous")
        );
        assert_eq!(form_error_code("effect_not_observed: x"), None);
        assert_eq!(form_error_code("value_truncated: x"), None);
        assert_eq!(form_error_code("webmcp_failed: x"), None);
        assert_eq!(form_error_code("read_only"), None);
    }

    #[test]
    fn test_truncation_is_a_warning_and_never_echoes() {
        let mut probe = text_probe("old", 5);
        assert!(truncation_warning(&probe, "01234").is_none());
        let w = truncation_warning(&probe, "0123456789").unwrap();
        assert!(w.starts_with("value_truncated"));
        probe.password = true;
        probe.value = Some("secret".into());
        let w = truncation_warning(&probe, "0123456789").unwrap();
        assert!(!w.contains("0123456789") && !w.contains("secret"));
        assert!(truncation_warning(&text_probe("", -1), "0123456789").is_none());
        // maxlength counts UTF-16 units.
        assert!(truncation_warning(&text_probe("", 2), "\u{1F600}").is_none());
        assert!(truncation_warning(&text_probe("", 1), "\u{1F600}").is_some());
    }

    #[test]
    fn test_fill_warning_is_never_an_error() {
        assert!(fill_warning(&text_probe("abc", -1), "abc").is_none());
        let w = fill_warning(&text_probe("", -1), "abc").unwrap();
        assert!(w.starts_with("effect_not_observed"));
        let w = fill_warning(&text_probe("(555) 123", -1), "555123").unwrap();
        assert!(w.contains("length 6") && w.contains("9"), "{w}");
        assert!(!w.contains("555") && !w.contains("(555) 123"), "{w}");
        // Same length, different value: still no value.
        let w = fill_warning(&text_probe("4111-1111", -1), "411111111").unwrap();
        assert!(!w.contains("4111") && w.contains("different value"), "{w}");
        // The browser keeps color values lowercase.
        let mut color = text_probe("#ff0000", -1);
        color.input_type = "color".into();
        assert!(fill_warning(&color, "#FF0000").is_none());
        // Password fields are never read back or echoed.
        let mut probe = text_probe("", -1);
        probe.password = true;
        assert!(fill_warning(&probe, "secret").is_none());
        // No value to compare, no warning.
        let mut none = text_probe("", -1);
        none.value = None;
        assert!(fill_warning(&none, "abc").is_none());
    }

    #[test]
    fn test_line_endings_do_not_cause_a_warning() {
        let mut area = text_probe("a\nb", -1);
        area.tag = "textarea".into();
        assert!(fill_warning(&area, "a\r\nb").is_none());
    }

    #[test]
    fn test_unknown_probe_never_warns_about_length() {
        let probe = FormProbe::unknown();
        assert!(truncation_warning(&probe, "any length at all").is_none());
    }

    #[tokio::test]
    async fn test_post_action_reads_go_through_one_helper() {
        let gone = "Ref @e1 is from a document that is no longer loaded (x)".to_string();
        for error in [
            "CDP error (Runtime.callFunctionOn): Cannot find context with specified id".to_string(),
            "Element not found: #cb".to_string(),
            gone,
        ] {
            let read = read_after(std::future::ready(Err::<bool, String>(error))).await;
            assert_eq!(
                unverified_warning(&read).as_deref(),
                Some(super::super::element::STATE_NOT_VERIFIED)
            );
        }
        let read = read_after(std::future::ready(Err::<bool, String>(
            "Could not inspect the element".to_string(),
        )))
        .await;
        assert!(unverified_warning(&read)
            .unwrap()
            .starts_with("could not verify the result"));
        assert!(unverified_warning(&AfterRead::Value(true)).is_none());
    }

    #[test]
    fn test_named_key_info_modifiers_and_code_names() {
        let cases: &[(&str, &str, &str, i32, Option<i32>)] = &[
            ("Shift", "Shift", "ShiftLeft", 16, Some(1)),
            ("ShiftRight", "Shift", "ShiftRight", 16, Some(2)),
            ("Control", "Control", "ControlLeft", 17, Some(1)),
            ("ctrl", "Control", "ControlLeft", 17, Some(1)),
            ("ControlRight", "Control", "ControlRight", 17, Some(2)),
            ("Alt", "Alt", "AltLeft", 18, Some(1)),
            ("AltRight", "Alt", "AltRight", 18, Some(2)),
            ("Meta", "Meta", "MetaLeft", 91, Some(1)),
            ("Cmd", "Meta", "MetaLeft", 91, Some(1)),
            ("MetaRight", "Meta", "MetaRight", 92, Some(2)),
            ("KeyW", "w", "KeyW", 87, None),
            ("keya", "a", "KeyA", 65, None),
            ("Digit7", "7", "Digit7", 55, None),
            ("Key", "Key", "Key", 0, None),
            ("KeyAB", "KeyAB", "KeyAB", 0, None),
            ("Digit10", "Digit10", "Digit10", 0, None),
            ("w", "w", "KeyW", 87, None),
            ("ArrowUp", "ArrowUp", "ArrowUp", 38, None),
        ];
        for &(input, key, code, vk, location) in cases {
            assert_eq!(
                (named_key_info(input), key_location(code)),
                ((key.to_string(), code.to_string(), vk), location),
                "input {input:?}"
            );
        }
    }

    #[test]
    fn test_modified_key_info_applies_shift_and_suppresses_command_text() {
        let check = |key: &str,
                     modifiers: Option<i32>,
                     want: (&str, &str, i32, Option<&str>, Option<&str>)| {
            let (want_key, want_code, want_vk, want_text, want_unmod) = want;
            assert_eq!(
                modified_key_info(key, modifiers),
                (
                    want_key.to_string(),
                    want_code.to_string(),
                    want_vk,
                    want_text.map(String::from),
                    want_unmod.map(String::from),
                ),
                "press {key} with modifiers {modifiers:?}"
            );
        };
        check("l", None, ("l", "KeyL", 76, Some("l"), Some("l")));
        check("l", Some(8), ("L", "KeyL", 76, Some("L"), Some("l")));
        check("1", Some(8), ("!", "Digit1", 49, Some("!"), Some("1")));
        check("/", Some(8), ("?", "Slash", 191, Some("?"), Some("/")));
        check("L", Some(8), ("L", "KeyL", 76, Some("L"), Some("L")));
        check(
            "Enter",
            Some(8),
            ("Enter", "Enter", 13, Some("\r"), Some("\r")),
        );
        check("l", Some(1), ("l", "KeyL", 76, None, None));
        check("l", Some(1 | 8), ("L", "KeyL", 76, None, None));
        check("a", Some(2), ("a", "KeyA", 65, None, None));
        check("a", Some(4), ("a", "KeyA", 65, None, None));
    }

    #[test]
    fn test_single_key_event_params() {
        let params = |key, event_type, modifiers| {
            serde_json::to_value(single_key_event_params(key, event_type, modifiers)).unwrap()
        };
        assert_eq!(
            params("Shift", "keyDown", Some(8)),
            serde_json::json!({
                "type": "keyDown", "key": "Shift", "code": "ShiftLeft",
                "windowsVirtualKeyCode": 16, "modifiers": 8, "location": 1
            })
        );
        assert_eq!(
            params("Shift", "keyUp", None),
            serde_json::json!({
                "type": "keyUp", "key": "Shift", "code": "ShiftLeft",
                "windowsVirtualKeyCode": 16, "location": 1
            })
        );
        assert_eq!(
            params("w", "keyDown", None),
            serde_json::json!({
                "type": "keyDown", "key": "w", "code": "KeyW", "text": "w",
                "unmodifiedText": "w", "windowsVirtualKeyCode": 87
            })
        );
        assert_eq!(
            params("w", "keyDown", Some(8)),
            serde_json::json!({
                "type": "keyDown", "key": "W", "code": "KeyW", "text": "W",
                "unmodifiedText": "w", "windowsVirtualKeyCode": 87, "modifiers": 8
            })
        );
        assert_eq!(
            params("w", "keyUp", None),
            serde_json::json!({
                "type": "keyUp", "key": "w", "code": "KeyW", "windowsVirtualKeyCode": 87
            })
        );
    }

    #[test]
    fn test_modifier_helpers() {
        assert_eq!(key_modifier_bit("Alt"), Some(1));
        assert_eq!(key_modifier_bit("AltRight"), Some(1));
        assert_eq!(key_modifier_bit("Control"), Some(2));
        assert_eq!(key_modifier_bit("ControlLeft"), Some(2));
        assert_eq!(key_modifier_bit("Meta"), Some(4));
        assert_eq!(key_modifier_bit("MetaRight"), Some(4));
        assert_eq!(key_modifier_bit("Shift"), Some(8));
        assert_eq!(key_modifier_bit("ShiftRight"), Some(8));
        assert_eq!(key_modifier_bit("KeyW"), None);
        assert_eq!(with_own_modifier_bit("Shift", None), Some(8));
        assert_eq!(with_own_modifier_bit("Control", Some(8)), Some(10));
        assert_eq!(with_own_modifier_bit("w", Some(2)), Some(2));
        assert_eq!(modifier_key_names(2 | 8), vec!["Control", "Shift"]);
        assert_eq!(
            modifier_key_names(1 | 2 | 4 | 8),
            vec!["Control", "Alt", "Shift", "Meta"]
        );
    }
}
