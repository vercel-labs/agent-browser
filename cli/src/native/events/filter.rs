use regex_lite::Regex;
use serde_json::Value;

/// Parses `<json-pointer>=<value>` and `<json-pointer>=<regex>` specs.
#[derive(Debug)]
pub struct Matchers(Vec<(String, Expected)>);

#[derive(Debug)]
enum Expected {
    Equals(String),
    Regex(Regex),
}

impl Matchers {
    /// Parses equals and regex specs; every spec must name a pointer starting with `/`.
    pub fn parse(equals: &[String], regexes: &[String]) -> Result<Self, String> {
        let mut items = Vec::with_capacity(equals.len() + regexes.len());
        for (index, spec) in equals.iter().chain(regexes).enumerate() {
            let (pointer, value) = spec
                .split_once('=')
                .ok_or_else(|| format!("Matcher '{spec}' must be <json-pointer>=<value>"))?;
            if !pointer.starts_with('/') {
                return Err(format!(
                    "Matcher '{spec}' must use a JSON pointer starting with '/'"
                ));
            }
            let expected = if index < equals.len() {
                Expected::Equals(value.to_owned())
            } else {
                Expected::Regex(Regex::new(value).map_err(|error| {
                    format!("Matcher '{spec}' has an invalid regular expression: {error}")
                })?)
            };
            items.push((pointer.to_owned(), expected));
        }
        Ok(Self(items))
    }

    /// Requires every matcher to hold, and a missing pointer never matches.
    pub fn matches(&self, projected: &Value) -> bool {
        self.0.iter().all(|(pointer, expected)| {
            let Some(value) = resolve(projected, pointer) else {
                return false;
            };
            let text = value
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| value.to_string());
            match expected {
                Expected::Equals(expected) => text == *expected,
                Expected::Regex(regex) => regex.is_match(&text),
            }
        })
    }
}

/// Resolves an RFC 6901 JSON pointer, including `~0` and `~1` escapes.
pub fn resolve<'a>(value: &'a Value, pointer: &str) -> Option<&'a Value> {
    pointer
        .split('/')
        .skip(1)
        .try_fold(value, |current, token| {
            let token = token.replace("~1", "/").replace("~0", "~");
            match current {
                Value::Object(object) => object.get(&token),
                Value::Array(items) => items.get(token.parse::<usize>().ok()?),
                _ => None,
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn scalar_pointer_regex_and_and_contracts() {
        let value = json!({"text": "alert", "number": 42, "flag": true, "nil": null, "a/b~c": "value", "args": ["first"]});
        for (spec, expected) in [
            ("/text=alert", true),
            ("/number=42", true),
            ("/flag=true", true),
            ("/nil=null", true),
            ("/a~1b~0c=value", true),
            ("/args/00=first", true),
            ("/text=confirm", false),
            ("/number=43", false),
            ("/flag=false", false),
            ("/args/0=second", false),
            ("/args/1=first", false),
            ("/args/no=first", false),
            ("/missing=null", false),
            ("/flag/child=true", false),
        ] {
            let matcher = Matchers::parse(&[spec.into()], &[]).unwrap();
            assert_eq!(matcher.matches(&value), expected, "{spec}");
        }
        let equals = ["/text=alert".into(), "/flag=true".into()];
        let regexes = ["/text=^a.*t$".into(), "/number=^4\\d$".into()];
        let matcher = Matchers::parse(&equals, &regexes).unwrap();
        for (value, expected) in [
            (value, true),
            (json!({"text": "alert", "flag": true, "number": 52}), false),
            (json!({}), false),
        ] {
            assert_eq!(matcher.matches(&value), expected);
        }
        assert!(Matchers::parse(&[], &[]).unwrap().matches(&Value::Null));
    }

    #[test]
    fn invalid_specs_keep_their_errors() {
        for (spec, suffix) in [
            ("/type", "must be <json-pointer>=<value>"),
            ("type=alert", "must use a JSON pointer starting with '/'"),
        ] {
            let error = format!("Matcher '{spec}' {suffix}");
            assert_eq!(Matchers::parse(&[spec.into()], &[]).unwrap_err(), error);
            assert_eq!(Matchers::parse(&[], &[spec.into()]).unwrap_err(), error);
        }
        let error = Matchers::parse(&[], &["/message=(".into()]).unwrap_err();
        assert!(error.starts_with("Matcher '/message=(' has an invalid regular expression: "));
    }
}
