//! Form actions shared by every engine: fill, type, check, uncheck and select.
//!
//! The decisions live here once: what may be refused before acting, which
//! element a check toggles, and what the page says afterwards. An engine only
//! supplies the primitives in [`FormControl`].

use serde_json::{json, Value};

use super::element::{not_actionable_error, STATE_NOT_VERIFIED};

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

/// Finds the element a check toggles for `el`: the checkbox, radio or
/// checkbox-like role itself, a label's checkbox or radio, or the one native
/// checkbox or radio a plain wrapper holds (also inside its open shadow root).
/// A link, button or other control, or an interactive element between the
/// target and its label, is never treated as the checkbox, so checking never
/// presses something else.
const CHECK_BOX_JS: &str = r#"
        const ariaRoles = ['checkbox', 'radio', 'switch', 'menuitemcheckbox', 'menuitemradio', 'option', 'treeitem'];
        const roleOf = (n) => ((n.getAttribute && n.getAttribute('role')) || '').split(/\s+/).find((r) => ariaRoles.includes(r));
        const boxOf = (n) => {
            const nodeTag = n.localName;
            if (nodeTag === 'input' && (n.type === 'checkbox' || n.type === 'radio')) return n;
            if (roleOf(n)) return n;
            const roles = ((n.getAttribute && n.getAttribute('role')) || '').split(/\s+/);
            const otherRoles = ['button', 'link', 'textbox', 'combobox', 'listbox', 'menuitem', 'searchbox', 'slider', 'spinbutton', 'tab'];
            const otherTags = ['button', 'a', 'area', 'select', 'textarea', 'input', 'summary', 'details', 'iframe', 'embed', 'object'];
            if (otherTags.includes(nodeTag) || roles.some((r) => otherRoles.includes(r))) return null;
            const interactive = 'button, a[href], area[href], input, select, textarea, summary, details, iframe, embed, object, [role], [onclick], [tabindex]:not([tabindex="-1"]), [contenteditable=""], [contenteditable="true"]';
            const isInteractive = (m) => !!m && ((m.matches && m.matches(interactive)) || typeof m.onclick === 'function');
            const label = nodeTag === 'label' ? n : n.closest && n.closest('label');
            if (label && label.control) {
                for (let cur = n; cur && cur !== label; cur = cur.parentElement) if (isInteractive(cur)) return null;
                const ctrl = label.control;
                return ctrl.localName === 'input' && (ctrl.type === 'checkbox' || ctrl.type === 'radio') ? ctrl : null;
            }
            const only = (root) => {
                const inputs = root.querySelectorAll('input[type="checkbox"], input[type="radio"]');
                if (inputs.length !== 1) return null;
                const others = 'button, a[href], area[href], input:not([type="checkbox"]):not([type="radio"]):not([type="hidden"]), select, textarea, summary, details, iframe, embed, object, [role], [onclick], [tabindex]:not([tabindex="-1"]), [contenteditable=""], [contenteditable="true"]';
                if (root.querySelector(others)) return null;
                for (const m of root.querySelectorAll('*')) if (m !== inputs[0] && typeof m.onclick === 'function') return null;
                return inputs[0];
            };
            return only(n) || (n.shadowRoot && only(n.shadowRoot)) || null;
        };
"#;

/// Reads what the form actions need to know about an element.
const PROBE_JS: &str = r#"function() {
        /*RETARGET*/
        /*BOX*/
        const tag = el.localName;
        const type = tag === 'input' ? el.type : '';
        const box = boxOf(el);
        let state = null;
        if (box) {
            const boxRole = roleOf(box);
            state = box.localName === 'input' ? box.checked
                : (boxRole === 'option' || boxRole === 'treeitem') && !box.hasAttribute('aria-checked')
                    ? box.getAttribute('aria-selected') === 'true'
                    : box.getAttribute('aria-checked') === 'true';
        }
        const boxRole = box && (roleOf(box) || box.type);
        let clickable = false;
        if (box) {
            const rect = box.getClientRects()[0];
            const style = getComputedStyle(box);
            clickable = !!(rect && rect.width > 0 && rect.height > 0 && style.display !== 'none' && style.visibility !== 'hidden' && parseFloat(style.opacity) > 0 && style.pointerEvents !== 'none');
        }
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
            clickable,
        };
    }"#;

const DOM_CLICK_JS: &str = "function() { this.click(); }";

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
    /// The element a check toggles has a visible box that takes the pointer.
    clickable: bool,
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

/// Why a call into the page gave no value.
#[derive(Debug)]
pub(crate) enum CallError {
    /// The element or the document it lived in is gone.
    Gone,
    /// The page script threw. Its text belongs to the page and is not repeated.
    Threw,
    /// A JavaScript dialog opened before the call returned.
    Dialog,
    /// Anything else, with the engine's message.
    Failed(String),
}

impl From<String> for CallError {
    fn from(error: String) -> Self {
        Self::Failed(error)
    }
}

impl From<CallError> for String {
    fn from(error: CallError) -> Self {
        match error {
            CallError::Gone => {
                "Element not found: the element is no longer in the page".to_string()
            }
            CallError::Threw => "the page script raised an error".to_string(),
            CallError::Dialog => "a JavaScript dialog is open".to_string(),
            CallError::Failed(error) => error,
        }
    }
}

/// The element a form action works on, as one engine reaches it. Everything a
/// form action decides is written once in this module against these calls.
pub(crate) trait FormControl {
    /// The selector or ref the caller named the element by.
    fn target(&self) -> &str;
    /// Calls `js` with `this` bound to the element and returns its value; a
    /// returned promise is awaited.
    async fn call(&mut self, js: &str, args: Vec<Value>) -> Result<Value, CallError>;
    /// Binds to the element `js` returns from the current one, or keeps the
    /// current one when it returns nothing.
    async fn bind(&mut self, js: &str) -> Result<(), CallError>;
    /// Binds to the element the target names now.
    async fn rebind(&mut self) -> Result<(), CallError>;
    /// Inserts `text` into the focused element that was just emptied, as one
    /// edit.
    async fn insert_text(&mut self, text: &str) -> Result<(), CallError>;
    /// Types `text` key by key at the focused element.
    async fn type_text(&mut self, text: &str, delay_ms: Option<u64>) -> Result<(), CallError>;
    /// Presses the bound element with the pointer and returns where, refusing
    /// an element another one covers.
    async fn click(&mut self) -> Result<(f64, f64), CallError>;
}

/// Runs a check that only reads the page. A script that throws cannot answer,
/// so the action goes ahead as it would without the check.
async fn inspect(
    control: &mut impl FormControl,
    js: &str,
    args: Vec<Value>,
) -> Result<Option<Value>, CallError> {
    match control.call(js, args).await {
        Ok(value) => Ok(Some(value)),
        Err(CallError::Threw) => Ok(None),
        Err(error) => Err(error),
    }
}

async fn probe(control: &mut impl FormControl) -> Result<FormProbe, CallError> {
    let js = PROBE_JS
        .replace("/*RETARGET*/", RETARGET_JS)
        .replace("/*BOX*/", CHECK_BOX_JS);
    let value = control.call(&js, Vec::new()).await?;
    serde_json::from_value(value)
        .map_err(|e| CallError::Failed(format!("Could not inspect the element: {e}")))
}

/// The probe before acting, or an unknown one when the element cannot be
/// inspected.
async fn probe_before(control: &mut impl FormControl) -> Result<FormProbe, CallError> {
    match probe(control).await {
        Err(CallError::Dialog) => Err(CallError::Dialog),
        result => Ok(result.unwrap_or_else(|_| FormProbe::unknown())),
    }
}

/// What a read after the action found. A document that is gone, or a target
/// that is no longer in the page, ends the verification without failing an
/// action that already happened.
enum AfterRead<T> {
    Value(T),
    Gone,
    Unreadable,
}

async fn read_after<T>(
    read: impl std::future::Future<Output = Result<T, CallError>>,
) -> Result<AfterRead<T>, CallError> {
    match read.await {
        Ok(value) => Ok(AfterRead::Value(value)),
        Err(CallError::Gone) => Ok(AfterRead::Gone),
        Err(CallError::Dialog) => Err(CallError::Dialog),
        Err(_) => Ok(AfterRead::Unreadable),
    }
}

/// The warning for a read that gave no value.
fn unverified_warning<T>(read: &AfterRead<T>) -> Option<String> {
    match read {
        AfterRead::Value(_) => None,
        AfterRead::Gone => Some(STATE_NOT_VERIFIED.to_string()),
        AfterRead::Unreadable => {
            Some("could not verify the result: the page could not be inspected".to_string())
        }
    }
}

/// The warning for a call that failed after the action was dispatched.
async fn failure_warning(error: CallError) -> Result<String, CallError> {
    let read = read_after(std::future::ready(Err::<(), CallError>(error))).await?;
    Ok(unverified_warning(&read).unwrap_or_default())
}

/// What a text action reports when the page did not end up as requested. The
/// action is not failed: pages apply values late or rewrite them.
#[derive(Debug, Default)]
pub struct FormOutcome {
    pub warning: Option<String>,
    /// A JavaScript dialog opened before the action finished.
    pub dialog_opened: bool,
}

impl FormOutcome {
    fn warn(warning: Option<String>) -> Self {
        Self {
            warning,
            dialog_opened: false,
        }
    }
}

/// Ends a text action: a dialog that opened is the outcome, not an error.
fn finish(result: Result<FormOutcome, CallError>) -> Result<FormOutcome, String> {
    match result {
        Err(CallError::Dialog) => Ok(FormOutcome {
            warning: None,
            dialog_opened: true,
        }),
        result => result.map_err(String::from),
    }
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
    control: &mut impl FormControl,
    verb: &str,
    guard: bool,
) -> Result<(), CallError> {
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
    let Some(value) = inspect(control, &js, Vec::new()).await? else {
        return Ok(());
    };
    if guard && value.get("landed").and_then(Value::as_bool) == Some(false) {
        let field = |key: &str| value.get(key).and_then(Value::as_str).unwrap_or("");
        return Err(CallError::Failed(form_error(
            "not_editable",
            format!(
                "Element {} ({}) cannot be {verb}: focusing it does not move focus, so the text would go to whatever was focused before",
                control.target(),
                describe_target(field("tag"), field("type"))
            ),
        )));
    }
    Ok(())
}

/// Empties the control so the next insert replaces its content: an input is
/// cleared, an editable region has its content selected and deleted. A field
/// that turned readonly on focus is refused with its value kept.
async fn clear_control(control: &mut impl FormControl, verb: &str) -> Result<(), CallError> {
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
    let Some(value) = inspect(control, &js, Vec::new()).await? else {
        return Ok(());
    };
    if value.get("refuse").and_then(Value::as_str) != Some("read_only") {
        return Ok(());
    }
    let field = |key: &str| value.get(key).and_then(Value::as_str).unwrap_or("");
    let (code, reason) = refusal("read_only", verb);
    Err(CallError::Failed(form_error(
        code,
        format!(
            "Element {} ({}) {reason}",
            control.target(),
            describe_target(field("tag"), field("type"))
        ),
    )))
}

/// Refuses `fill` or `type` on a target that cannot take the text, before
/// anything is changed. The message names the target as the caller passed it.
async fn precheck(control: &mut impl FormControl, mode: &str) -> Result<(), CallError> {
    let js = PRECHECK_JS.replace("/*RETARGET*/", RETARGET_JS);
    let Some(value) = inspect(control, &js, vec![json!(mode)]).await? else {
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
    Err(CallError::Failed(form_error(
        code,
        format!("Element {} ({described}) {reason}", control.target()),
    )))
}

struct ValueSet {
    snapped: Option<String>,
    stored: Option<String>,
}

/// Sets date, time, color and range inputs directly: browsers drop inserted
/// text there. Refuses a value the input would sanitize away. A color input
/// takes any CSS color it stores as that color's `#rrggbb`; one it stores as
/// another color is refused. The result is later compared with the value the
/// browser stored, not the text given.
async fn set_value_input(
    control: &mut impl FormControl,
    value: &str,
) -> Result<ValueSet, CallError> {
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
    let value = control.call(&js, vec![json!(value)]).await?;
    if value.get("malformed").and_then(Value::as_bool) == Some(true) {
        return Err(CallError::Failed(form_error(
            "malformed_value",
            malformed_message(&value),
        )));
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

/// Binds to the control a target stands for: a label resolves to its control,
/// and a part of a date or time input to the input. Every step of `fill` and
/// `type` acts on this one element; one that cannot be resolved stays as is.
async fn bind_control(control: &mut impl FormControl) -> Result<(), CallError> {
    match control
        .bind(&format!("function() {{ {RETARGET_JS} return el; }}"))
        .await
    {
        Err(CallError::Dialog) => Err(CallError::Dialog),
        _ => Ok(()),
    }
}

/// Replaces the control's value with `value`: a date, time, color or range
/// input is set directly, anything else is focused, emptied and given the
/// text as one insert.
pub(crate) async fn fill(
    control: &mut impl FormControl,
    value: &str,
) -> Result<FormOutcome, String> {
    finish(fill_steps(control, value).await)
}

async fn fill_steps(control: &mut impl FormControl, value: &str) -> Result<FormOutcome, CallError> {
    bind_control(control).await?;
    precheck(control, "fill").await?;
    let before = probe_before(control).await?;
    if before.value_input {
        let set = match set_value_input(control, value).await {
            Ok(set) => set,
            Err(CallError::Failed(error)) if form_error_code(&error).is_some() => {
                return Err(CallError::Failed(error))
            }
            Err(error) => return Ok(FormOutcome::warn(Some(failure_warning(error).await?))),
        };
        if set.snapped.is_some() {
            return Ok(FormOutcome::warn(set.snapped));
        }
        let expected = set.stored.as_deref().unwrap_or(value.trim());
        let read = read_after(probe(control)).await?;
        let warning = match &read {
            AfterRead::Value(after) => fill_warning(after, expected),
            other => unverified_warning(other),
        };
        return Ok(FormOutcome::warn(warning));
    }

    focus_target(control, "filled", true).await?;
    clear_control(control, "filled").await?;
    control.insert_text(value).await?;

    let read = read_after(probe(control)).await?;
    let warning = match &read {
        AfterRead::Value(after) => {
            truncation_warning(&before, value).or_else(|| fill_warning(after, value))
        }
        other => unverified_warning(other),
    };
    Ok(FormOutcome::warn(warning))
}

/// Focuses the control and types `text` key by key, emptying it first with
/// `clear`.
pub(crate) async fn type_text(
    control: &mut impl FormControl,
    text: &str,
    clear: bool,
    delay_ms: Option<u64>,
) -> Result<FormOutcome, String> {
    finish(type_steps(control, text, clear, delay_ms).await)
}

async fn type_steps(
    control: &mut impl FormControl,
    text: &str,
    clear: bool,
    delay_ms: Option<u64>,
) -> Result<FormOutcome, CallError> {
    bind_control(control).await?;

    // Control characters alone are key events, which any focused target can
    // receive; any text needs a target that takes it.
    let keys_only = !text.is_empty() && text.chars().all(|c| matches!(c, '\n' | '\r' | '\t'));
    // Clearing destroys the value, so `--clear` is checked like `fill`, even
    // for key-only text.
    if clear {
        precheck(control, "type-clear").await?;
    } else if !keys_only {
        precheck(control, "type").await?;
    }
    let before = probe_before(control).await?;

    let verb = if clear {
        "cleared and typed into"
    } else {
        "typed into"
    };
    focus_target(control, verb, clear || !keys_only).await?;
    if clear {
        clear_control(control, verb).await?;
    }

    // A field may select its content on focus; that content is replaced by
    // the typed text, so it does not count towards the length held afterwards.
    let before = FormProbe {
        selected: probe_before(control).await?.selected,
        ..before
    };

    control.type_text(text, delay_ms).await?;

    let after = match read_after(probe(control)).await? {
        AfterRead::Value(after) => after,
        other => return Ok(FormOutcome::warn(unverified_warning(&other))),
    };
    Ok(FormOutcome::warn(type_read_warning(
        &before, &after, text, clear,
    )))
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

/// Matching nothing must be an error, not a silent success: an agent that
/// selects a misspelled option otherwise sees "Done", and only discovers the
/// page state is wrong after more commands. List what was available.
///
/// Native <select> keeps its own path. For a div-based ARIA combobox/listbox
/// (Radix, Headless UI, ui-sdk) there is no `this.options`, so open the widget
/// when it is not already expanded, wait for its role=option items, and click
/// the ones whose accessible name or value matches.
const SELECT_JS: &str = r#"async function(vals, by) {
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
        }"#;

/// Selects `values` in a native select or an ARIA combobox or listbox.
pub(crate) async fn select(
    control: &mut impl FormControl,
    values: &[String],
    by: Option<&str>,
) -> Result<(), SelectError> {
    let page = match control
        .call(SELECT_JS, vec![json!(values), json!(by)])
        .await
    {
        Ok(page) => page,
        Err(CallError::Dialog) => return Ok(()),
        Err(CallError::Threw) => {
            return Err("select failed: the page script raised an error"
                .to_string()
                .into())
        }
        Err(error) => return Err(String::from(error).into()),
    };
    if let Some((code, message)) = select_failure(&page, values, by) {
        let message = match code {
            Some(code) => form_error(code, message),
            None => message,
        };
        return Err(SelectError {
            message,
            options: available_options(&page),
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

/// Checks (`want`) or unchecks the target. The element pressed is the
/// checkbox the target stands for, never a label or wrapper around it, so a
/// link or button that shares the label is not activated. A checkbox with no
/// visible box is a hidden native input and gets a DOM click.
pub(crate) async fn set_checked(
    control: &mut impl FormControl,
    want: bool,
) -> Result<ToggleOutcome, String> {
    match toggle(control, want).await {
        Err(CallError::Dialog) => Ok(ToggleOutcome::default()),
        result => result.map_err(String::from),
    }
}

async fn toggle(control: &mut impl FormControl, want: bool) -> Result<ToggleOutcome, CallError> {
    let verb = if want { "checked" } else { "unchecked" };
    let probe_now = match probe(control).await {
        Ok(probe_now) => probe_now,
        Err(CallError::Dialog) => return Err(CallError::Dialog),
        Err(_) => return toggle_unprobed(control, want).await,
    };
    if !probe_now.checkable {
        return Err(CallError::Failed(form_error(
            "not_checkable",
            format!(
                "Element {} is not a checkbox, radio or element with a checkbox-like role, so it cannot be {verb}",
                probe_now.describe()
            ),
        )));
    }
    if probe_now.checked.unwrap_or(false) == want {
        return Ok(ToggleOutcome::default());
    }
    if probe_now.disabled || probe_now.box_disabled {
        return Err(CallError::Failed(form_error(
            "disabled",
            format!(
                "Element {} is disabled and cannot be {verb}",
                probe_now.describe()
            ),
        )));
    }
    if !want && probe_now.radio {
        return Err(CallError::Failed(form_error(
            "not_checkable",
            "A checked radio cannot be unchecked by clicking; select another radio in its group instead",
        )));
    }

    control
        .bind(&format!(
            "function() {{ {RETARGET_JS} {CHECK_BOX_JS} return boxOf(el); }}"
        ))
        .await?;
    let mut outcome = ToggleOutcome::default();
    if probe_now.clickable {
        outcome.position = Some(control.click().await?);
    } else if probe_now.native {
        control.call(DOM_CLICK_JS, Vec::new()).await?;
    } else {
        return Err(CallError::Failed(not_actionable_error(
            control.target(),
            "not visible",
        )));
    }

    // A native input whose pointer press missed (an overlay) is retried with a
    // DOM click. A custom control is not: a second click would undo a first
    // one the page applies late. The click has already happened, so a read
    // that fails afterwards ends the verification without failing the action.
    let mut after = read_after(probe_target(control)).await?;
    if let AfterRead::Value(probe_now) = &after {
        if probe_now.checked.unwrap_or(false) != want && probe_now.native {
            let retried = read_after(async {
                control
                    .bind(&format!(
                        "function() {{ {RETARGET_JS} {CHECK_BOX_JS} return boxOf(el); }}"
                    ))
                    .await?;
                control.call(DOM_CLICK_JS, Vec::new()).await
            })
            .await?;
            after = match retried {
                AfterRead::Value(_) => read_after(probe_target(control)).await?,
                AfterRead::Gone => AfterRead::Gone,
                AfterRead::Unreadable => AfterRead::Unreadable,
            };
        }
    }
    match after {
        AfterRead::Value(probe_now) => {
            if probe_now.checked.unwrap_or(false) != want {
                outcome.warning = Some(format!(
                    "effect_not_observed: {} is still {} after clicking; the page may apply the change later",
                    probe_now.describe(),
                    if want { "unchecked" } else { "checked" }
                ));
            }
        }
        AfterRead::Gone => outcome.verified = false,
        AfterRead::Unreadable => outcome.warning = unverified_warning(&AfterRead::<()>::Unreadable),
    }
    Ok(outcome)
}

/// Probes the element the target names now, which a click may have replaced.
async fn probe_target(control: &mut impl FormControl) -> Result<FormProbe, CallError> {
    control.rebind().await?;
    probe(control).await
}

const LABEL_CLICK_JS: &str = r#"function() {
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

/// Toggles without the preconditions when the element cannot be inspected:
/// click when the state differs, retry once with a DOM click.
async fn toggle_unprobed(
    control: &mut impl FormControl,
    want: bool,
) -> Result<ToggleOutcome, CallError> {
    if checked_state(control).await? == want {
        return Ok(ToggleOutcome::default());
    }
    let mut outcome = ToggleOutcome {
        position: Some(control.click().await?),
        ..ToggleOutcome::default()
    };
    let mut now = read_after(checked_state(control)).await?;
    if matches!(now, AfterRead::Value(current) if current != want) {
        let retried = read_after(control.call(LABEL_CLICK_JS, Vec::new())).await?;
        now = match retried {
            AfterRead::Value(_) => read_after(checked_state(control)).await?,
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

async fn checked_state(control: &mut impl FormControl) -> Result<bool, CallError> {
    control.rebind().await?;
    let value = control
        .call(super::element::ELEMENT_CHECKED_JS, Vec::new())
        .await?;
    Ok(value.as_bool().unwrap_or(false))
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
