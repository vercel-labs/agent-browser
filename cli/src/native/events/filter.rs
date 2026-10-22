//! Field matchers for the CDP method tap: equals and regex on JSON pointers.

use regex_lite::Regex;
use serde_json::Value;

/// Parses `<json-pointer>=<value>` and `<json-pointer>=<regex>` specs.
#[derive(Debug)]
pub struct Matchers {
    items: Vec<Matcher>,
}

#[derive(Debug)]
struct Matcher {
    pointer: String,
    expected: Expected,
}

#[derive(Debug)]
enum Expected {
    Equals(String),
    Regex(Regex),
}

impl Matchers {
    /// Parses equals and regex specs; every spec must name a pointer starting with `/`.
    pub fn parse(equals: &[String], regexes: &[String]) -> Result<Self, String> {
        let mut items = Vec::with_capacity(equals.len() + regexes.len());
        for spec in equals {
            let (pointer, value) = split_spec(spec)?;
            items.push(Matcher {
                pointer: pointer.to_owned(),
                expected: Expected::Equals(value.to_owned()),
            });
        }
        for spec in regexes {
            let (pointer, pattern) = split_spec(spec)?;
            let regex = Regex::new(pattern).map_err(|error| {
                format!("Matcher '{spec}' has an invalid regular expression: {error}")
            })?;
            items.push(Matcher {
                pointer: pointer.to_owned(),
                expected: Expected::Regex(regex),
            });
        }
        Ok(Self { items })
    }

    /// Requires every matcher to hold, and a missing pointer never matches.
    pub fn matches(&self, projected: &Value) -> bool {
        self.items.iter().all(|matcher| {
            let Some(value) = resolve(projected, &matcher.pointer) else {
                return false;
            };
            let text = string_form(value);
            match &matcher.expected {
                Expected::Equals(expected) => text == *expected,
                Expected::Regex(regex) => regex.is_match(&text),
            }
        })
    }
}

/// Resolves an RFC 6901 JSON pointer, including `~0` and `~1` escapes.
pub fn resolve<'a>(value: &'a Value, pointer: &str) -> Option<&'a Value> {
    let mut current = value;
    for token in pointer.split('/').skip(1) {
        let token = token.replace("~1", "/").replace("~0", "~");
        current = match current {
            Value::Object(object) => object.get(&token)?,
            Value::Array(items) => items.get(token.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(current)
}

fn split_spec(spec: &str) -> Result<(&str, &str), String> {
    let (pointer, value) = spec
        .split_once('=')
        .ok_or_else(|| format!("Matcher '{spec}' must be <json-pointer>=<value>"))?;
    if !pointer.starts_with('/') {
        return Err(format!(
            "Matcher '{spec}' must use a JSON pointer starting with '/'"
        ));
    }
    Ok((pointer, value))
}

fn string_form(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::Matchers;

    fn parse(equals: &[&str], regexes: &[&str]) -> Result<Matchers, String> {
        Matchers::parse(
            &equals.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            &regexes.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        )
    }

    #[test]
    fn equals_matches_strings_and_json_text() {
        let matchers = parse(&["/params/type=alert"], &[]).unwrap();
        assert!(matchers.matches(&json!({"params": {"type": "alert"}})));
        assert!(!matchers.matches(&json!({"params": {"type": "confirm"}})));

        let number = parse(&["/params/count=42"], &[]).unwrap();
        assert!(number.matches(&json!({"params": {"count": 42}})));
        assert!(!number.matches(&json!({"params": {"count": "43"}})));

        let boolean = parse(&["/params/flag=true"], &[]).unwrap();
        assert!(boolean.matches(&json!({"params": {"flag": true}})));
        assert!(!boolean.matches(&json!({"params": {"flag": false}})));
    }

    #[test]
    fn regex_runs_on_the_same_string_form() {
        let matchers = parse(&[], &["/params/url=^https://example\\.com/"]).unwrap();
        assert!(matchers.matches(&json!({"params": {"url": "https://example.com/a"}})));
        assert!(!matchers.matches(&json!({"params": {"url": "http://example.com/a"}})));

        let number = parse(&[], &["/params/count=^4\\d$"]).unwrap();
        assert!(number.matches(&json!({"params": {"count": 42}})));
    }

    #[test]
    fn every_matcher_must_hold_and_a_missing_pointer_fails() {
        let matchers = parse(&["/params/type=alert"], &["/params/message=^Hello"]).unwrap();
        assert!(matchers.matches(&json!({"params": {"type": "alert", "message": "Hello there"}})));
        assert!(!matchers.matches(&json!({"params": {"type": "alert", "message": "Bye"}})));
        assert!(!matchers.matches(&json!({"params": {"type": "alert"}})));
        assert!(!matchers.matches(&json!({})));
    }

    #[test]
    fn pointers_support_escapes_and_array_indexes() {
        let escaped = parse(&["/a~1b~0c=value"], &[]).unwrap();
        assert!(escaped.matches(&json!({"a/b~c": "value"})));

        let indexed = parse(&["/args/0/value=first"], &[]).unwrap();
        assert!(indexed.matches(&json!({"args": [{"value": "first"}]})));
        assert!(!indexed.matches(&json!({"args": [{"value": "second"}]})));
    }

    #[test]
    fn invalid_specs_are_rejected_with_the_spec() {
        let missing_equals = parse(&["/params/type"], &[]).unwrap_err();
        assert!(missing_equals.contains("/params/type"));
        assert!(missing_equals.contains("<json-pointer>=<value>"));

        let relative = parse(&["params/type=alert"], &[]).unwrap_err();
        assert!(relative.contains("params/type=alert"));
        assert!(relative.contains("starting with '/'"));

        let bad_regex = parse(&[], &["/params/message=("]).unwrap_err();
        assert!(bad_regex.contains("invalid regular expression"));
    }

    #[test]
    fn empty_matcher_list_matches_everything() {
        assert!(parse(&[], &[]).unwrap().matches(&json!({})));
    }
}
