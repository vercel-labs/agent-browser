//! Placeholders in a saved request: `{{name}}` is a param and `{{@id.path}}`
//! a value from an earlier response.
//!
//! A template is parsed once into parts. Binding walks those parts and
//! appends each value as data, encoded for the slot it lands in, so text that
//! was inserted is never read as a placeholder again.

use super::*;

/// Where a value lands, which decides how it is encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    /// A URL query component or path segment.
    Url,
    /// An `application/x-www-form-urlencoded` body.
    Form,
    /// A string inside a JSON body; serialization escapes it.
    Json,
    /// Literal text: a header value or a body that is neither JSON nor a form.
    Raw,
    /// A multipart body, where a value must not end a part.
    Multipart,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Part {
    Lit(String),
    Param(String),
    Ref { from: String, path: String },
}

pub fn parse(text: &str) -> Vec<Part> {
    let mut parts = Vec::new();
    let mut lit = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        let Some(len) = rest[start..].find("}}") else {
            break;
        };
        lit.push_str(&rest[..start]);
        let inner = &rest[start + 2..start + len];
        if !lit.is_empty() {
            parts.push(Part::Lit(std::mem::take(&mut lit)));
        }
        parts.push(match inner.strip_prefix('@') {
            Some(reference) => match reference.split_once('.') {
                Some((from, path)) => Part::Ref {
                    from: from.to_string(),
                    path: path.to_string(),
                },
                None => Part::Ref {
                    from: reference.to_string(),
                    path: String::new(),
                },
            },
            None => Part::Param(inner.to_string()),
        });
        rest = &rest[start + len + 2..];
    }
    lit.push_str(rest);
    if !lit.is_empty() {
        parts.push(Part::Lit(lit));
    }
    parts
}

fn encode(value: &str, slot: Slot) -> String {
    match slot {
        Slot::Url | Slot::Form => urlencoding::encode(value).into_owned(),
        Slot::Json | Slot::Raw | Slot::Multipart => value.to_string(),
    }
}

/// A value that could change the structure of a part it is written into as
/// is: a line break in a header, plain text or multipart body, or the
/// boundary of a multipart body.
fn breaks_structure(value: &str, slot: Slot, boundary: Option<&str>) -> Option<&'static str> {
    if !matches!(slot, Slot::Raw | Slot::Multipart) {
        return None;
    }
    if value.contains(['\r', '\n']) {
        return Some("a line break");
    }
    if boundary.is_some_and(|b| !b.is_empty() && value.contains(b)) {
        return Some("the multipart boundary");
    }
    None
}

fn refuse(what: &str, found: &str) -> String {
    format!(
        "{} has {}, which could break the structure of the request; nothing was sent",
        what, found
    )
}

/// Fill the params. Each value becomes literal data; references stay.
pub fn bind_params(
    parts: &[Part],
    slot: Slot,
    boundary: Option<&str>,
    params: &HashMap<String, String>,
) -> Result<Vec<Part>, String> {
    parts
        .iter()
        .map(|part| match part {
            Part::Param(name) => {
                let value = params
                    .get(name)
                    .ok_or_else(|| format!("missing --param {}", name))?;
                if let Some(found) = breaks_structure(value, slot, boundary) {
                    return Err(refuse(&format!("param {}", name), found));
                }
                Ok(Part::Lit(encode(value, slot)))
            }
            other => Ok(other.clone()),
        })
        .collect()
}

/// Fill the references from the responses received so far.
pub fn bind_refs(
    parts: &[Part],
    slot: Slot,
    boundary: Option<&str>,
    bodies: &HashMap<String, Value>,
) -> Result<String, String> {
    let mut out = String::new();
    for part in parts {
        match part {
            Part::Lit(text) => out.push_str(text),
            Part::Param(name) => out.push_str(&format!("{{{{{}}}}}", name)),
            Part::Ref { from, path } => {
                if path.is_empty() {
                    return Err(format!("bad reference {}", from));
                }
                let body = bodies
                    .get(from)
                    .ok_or(format!("reference to {} before it ran", from))?;
                let value = lookup(body, path)
                    .map(as_text)
                    .ok_or(format!("{} no longer has '{}'", from, path))?;
                if let Some(found) = breaks_structure(&value, slot, boundary) {
                    return Err(refuse(&format!("the value of {}.{}", from, path), found));
                }
                out.push_str(&encode(&value, slot));
            }
        }
    }
    Ok(out)
}

/// Bind what is known in one pass. A param with no value and a reference
/// stay in the text as placeholders.
pub fn bind_known(parts: &[Part], slot: Slot, values: &HashMap<String, String>) -> String {
    parts
        .iter()
        .map(|part| match part {
            Part::Lit(text) => text.clone(),
            Part::Param(name) => values
                .get(name)
                .map(|v| encode(v, slot))
                .unwrap_or_else(|| format!("{{{{{}}}}}", name)),
            other => show(std::slice::from_ref(other)),
        })
        .collect()
}

/// The template as text, with what is still unbound shown as placeholders.
pub fn show(parts: &[Part]) -> String {
    parts
        .iter()
        .map(|part| match part {
            Part::Lit(text) => text.clone(),
            Part::Param(name) => format!("{{{{{}}}}}", name),
            Part::Ref { from, path } => format!("{{{{@{}.{}}}}}", from, path),
        })
        .collect()
}

/// A JSON body whose strings are templates.
#[derive(Clone, Debug)]
pub enum JsonTpl {
    Str(Vec<Part>),
    Arr(Vec<JsonTpl>),
    Obj(Vec<(String, JsonTpl)>),
    Other(Value),
}

impl JsonTpl {
    fn of(value: &Value) -> JsonTpl {
        match value {
            Value::String(s) => JsonTpl::Str(parse(s)),
            Value::Array(items) => JsonTpl::Arr(items.iter().map(JsonTpl::of).collect()),
            Value::Object(map) => JsonTpl::Obj(
                map.iter()
                    .map(|(k, v)| (k.clone(), JsonTpl::of(v)))
                    .collect(),
            ),
            other => JsonTpl::Other(other.clone()),
        }
    }

    fn map(&self, f: &dyn Fn(&[Part]) -> Result<Value, String>) -> Result<Value, String> {
        Ok(match self {
            JsonTpl::Str(parts) => f(parts)?,
            JsonTpl::Arr(items) => {
                Value::Array(items.iter().map(|i| i.map(f)).collect::<Result<_, _>>()?)
            }
            JsonTpl::Obj(fields) => Value::Object(
                fields
                    .iter()
                    .map(|(k, v)| Ok((k.clone(), v.map(f)?)))
                    .collect::<Result<_, String>>()?,
            ),
            JsonTpl::Other(v) => v.clone(),
        })
    }

    fn bind(&self, params: &HashMap<String, String>) -> Result<JsonTpl, String> {
        Ok(match self {
            JsonTpl::Str(parts) => JsonTpl::Str(bind_params(parts, Slot::Json, None, params)?),
            JsonTpl::Arr(items) => JsonTpl::Arr(
                items
                    .iter()
                    .map(|i| i.bind(params))
                    .collect::<Result<_, _>>()?,
            ),
            JsonTpl::Obj(fields) => JsonTpl::Obj(
                fields
                    .iter()
                    .map(|(k, v)| Ok((k.clone(), v.bind(params)?)))
                    .collect::<Result<_, String>>()?,
            ),
            JsonTpl::Other(v) => JsonTpl::Other(v.clone()),
        })
    }
}

/// A request body as a template.
#[derive(Clone, Debug)]
pub enum Body {
    Json(JsonTpl),
    /// Text with the slot it is written in and, for multipart, its boundary.
    Text(Vec<Part>, Slot, Option<String>),
}

/// How a body that is not JSON is written: a form by its content type or by
/// its shape, multipart by its content type or its shape, otherwise as text.
pub fn text_slot(text: &str, headers: &[(String, String)]) -> (Slot, Option<String>) {
    let content_type = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
        .map(|(_, v)| v.as_str())
        .unwrap_or("");
    let lower = content_type.to_ascii_lowercase();
    let boundary_of_header = || {
        let at = lower.find("boundary=")?;
        let raw = content_type[at + 9..].split(';').next()?.trim();
        Some(raw.trim_matches('"').to_string())
    };
    let first_line = text.lines().next().unwrap_or("");
    let looks_multipart = first_line.starts_with("--")
        && text.contains("\r\n")
        && text.contains("Content-Disposition");
    if lower.contains("multipart/") || looks_multipart {
        let boundary = boundary_of_header().or_else(|| {
            looks_multipart.then(|| first_line.trim_start_matches('-').trim().to_string())
        });
        return (Slot::Multipart, boundary);
    }
    if lower.contains("application/x-www-form-urlencoded") || looks_like_form(text) {
        return (Slot::Form, None);
    }
    (Slot::Raw, None)
}

/// `k=v(&k=v)*` on one line, however the request labels it.
fn looks_like_form(text: &str) -> bool {
    !text.is_empty()
        && !text.contains(['\r', '\n'])
        && text
            .split('&')
            .all(|pair| pair.split_once('=').is_some_and(|(k, _)| !k.is_empty()))
}

impl Body {
    pub fn parse(text: &str, headers: &[(String, String)]) -> Body {
        match serde_json::from_str::<Value>(text) {
            Ok(value) => Body::Json(JsonTpl::of(&value)),
            Err(_) => {
                let (slot, boundary) = text_slot(text, headers);
                Body::Text(parse(text), slot, boundary)
            }
        }
    }

    pub fn bind_params(&self, params: &HashMap<String, String>) -> Result<Body, String> {
        Ok(match self {
            Body::Json(tpl) => Body::Json(tpl.bind(params)?),
            Body::Text(parts, slot, boundary) => Body::Text(
                bind_params(parts, *slot, boundary.as_deref(), params)?,
                *slot,
                boundary.clone(),
            ),
        })
    }

    pub fn bind_refs(&self, bodies: &HashMap<String, Value>) -> Result<String, String> {
        match self {
            Body::Json(tpl) => {
                let value = tpl
                    .map(&|parts| Ok(Value::String(bind_refs(parts, Slot::Json, None, bodies)?)))?;
                serde_json::to_string(&value).map_err(|e| e.to_string())
            }
            Body::Text(parts, slot, boundary) => {
                bind_refs(parts, *slot, boundary.as_deref(), bodies)
            }
        }
    }

    /// The body as text, with references still shown as placeholders.
    pub fn show(&self) -> String {
        match self {
            Body::Json(tpl) => tpl
                .map(&|parts| Ok(Value::String(show(parts))))
                .and_then(|v| serde_json::to_string(&v).map_err(|e| e.to_string()))
                .unwrap_or_default(),
            Body::Text(parts, _, _) => show(parts),
        }
    }
}

/// The templates of a request's URL, headers and body.
#[derive(Clone, Debug)]
pub struct Parts {
    pub url: Vec<Part>,
    pub body: Option<Body>,
}

impl Parts {
    /// Templates read from a request's text, for a request that was not
    /// built from a flow file.
    pub fn of_text(url: &str, body: Option<&str>, headers: &[(String, String)]) -> Parts {
        Parts {
            url: parse(url),
            body: body.map(|b| Body::parse(b, headers)),
        }
    }
}

// ---------------------------------------------------------------------------
// Save: find a recorded value in its slot and put the placeholder back.
// ---------------------------------------------------------------------------

fn needles(value: &str, slot: Slot) -> Vec<String> {
    let mut all = vec![value.to_string()];
    if matches!(slot, Slot::Url | Slot::Form) {
        let encoded = urlencoding::encode(value).into_owned();
        all.push(encoded.replace("%20", "+"));
        all.push(encoded);
    }
    all.retain(|n| !n.is_empty());
    all
}

/// Replace each recorded param value in `text` with its placeholder, in one
/// pass, looking for the value in the form its slot writes it.
pub fn templatize(text: &str, params: &[(String, String)], slot: Slot) -> String {
    let mut candidates: Vec<(String, &str)> = params
        .iter()
        .flat_map(|(name, value)| {
            needles(value, slot)
                .into_iter()
                .map(move |n| (n, name.as_str()))
        })
        .collect();
    candidates.sort_by_key(|(n, _)| std::cmp::Reverse(n.len()));
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while at < text.len() {
        let rest = &text[at..];
        match candidates
            .iter()
            .find(|(n, _)| rest.starts_with(n.as_str()))
        {
            Some((n, name)) => {
                out.push_str(&format!("{{{{{}}}}}", name));
                at += n.len();
            }
            None => {
                let c = rest.chars().next().unwrap_or_default();
                out.push(c);
                at += c.len_utf8().max(1);
            }
        }
    }
    out
}

/// Templatize a request body in its own slot. The text of a GraphQL
/// operation stays literal, so a param fills `variables` and other fields
/// but never the operation itself.
pub fn templatize_body(
    body: &str,
    params: &[(String, String)],
    headers: &[(String, String)],
) -> String {
    let Ok(mut parsed) = serde_json::from_str::<Value>(body) else {
        return templatize(body, params, text_slot(body, headers).0);
    };
    let is_op = |v: &Value| v.get("query").is_some_and(|q| q.is_string());
    let graphql = match &parsed {
        Value::Array(items) => !items.is_empty() && items.iter().all(is_op),
        other => is_op(other),
    };
    let before = parsed.clone();
    let strings = |v: &mut Value| {
        let _ = map_strings(v, &|t| Ok(templatize(t, params, Slot::Json)));
    };
    if graphql {
        let ops: Vec<&mut Value> = match &mut parsed {
            Value::Array(items) => items.iter_mut().collect(),
            other => vec![other],
        };
        for op in ops {
            if let Some(map) = op.as_object_mut() {
                for (key, value) in map.iter_mut() {
                    if key != "query" {
                        strings(value);
                    }
                }
            }
        }
    } else {
        strings(&mut parsed);
    }
    if parsed == before {
        return body.to_string();
    }
    serde_json::to_string(&parsed).unwrap_or_else(|_| body.to_string())
}
