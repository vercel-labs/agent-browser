use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::Path;

/// Ensure `packages/dashboard/out/` exists so `rust-embed` doesn't fail during
/// Rust-only dev builds where the dashboard hasn't been built. The placeholder
/// `index.html` is only written when the directory is completely absent.
fn ensure_dashboard_dir() {
    let dashboard_out = Path::new("../packages/dashboard/out");
    println!("cargo:rerun-if-changed=../packages/dashboard/out");
    if !dashboard_out.join("index.html").exists() {
        let _ = fs::create_dir_all(dashboard_out);
        let _ = fs::write(
            dashboard_out.join("index.html"),
            "<!DOCTYPE html><html><body><p>Dashboard not built. Run: cd packages/dashboard &amp;&amp; pnpm build</p></body></html>\n",
        );
    }
}

fn main() {
    ensure_dashboard_dir();

    let protocol_dir = Path::new("cdp-protocol");
    let out_dir = env::var("OUT_DIR").unwrap();
    let out_path = Path::new(&out_dir).join("cdp_generated.rs");

    let browser_path = protocol_dir.join("browser_protocol.json");
    let js_path = protocol_dir.join("js_protocol.json");

    if !browser_path.exists() && !js_path.exists() {
        fs::write(
            &out_path,
            "// No protocol JSON files found in cdp-protocol/\n",
        )
        .unwrap();
        return;
    }

    let mut all_domains: Vec<Domain> = Vec::new();

    for path in [&browser_path, &js_path] {
        if !path.exists() {
            continue;
        }
        println!("cargo:rerun-if-changed={}", path.display());
        let content = fs::read_to_string(path).unwrap();
        let protocol: ProtocolSpec = match serde_json::from_str(&content) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("cargo:warning=Failed to parse {}: {}", path.display(), e);
                continue;
            }
        };
        all_domains.extend(protocol.domains);
    }

    // Collect all known type IDs per domain for cross-domain resolution
    let mut domain_types: std::collections::HashMap<String, HashSet<String>> =
        std::collections::HashMap::new();
    for domain in &all_domains {
        let mut types = HashSet::new();
        for td in &domain.types {
            types.insert(td.id.clone());
        }
        domain_types.insert(domain.domain.clone(), types);
    }

    // Known recursive struct fields that need Box wrapping
    let recursive_fields: HashSet<(&str, &str, &str)> = [
        ("DOM", "Node", "contentDocument"),
        ("DOM", "Node", "templateContent"),
        ("DOM", "Node", "importedDocument"),
        ("Accessibility", "AXNode", "sources"),
        ("Runtime", "StackTrace", "parent"),
    ]
    .into_iter()
    .collect();

    let mut output = String::new();
    output.push_str("use serde::{Deserialize, Serialize};\n\n");

    for domain in &all_domains {
        generate_domain(domain, &domain_types, &recursive_fields, &mut output);
    }

    fs::write(&out_path, &output).unwrap();
}

#[allow(dead_code)]
#[derive(serde::Deserialize)]
struct ProtocolSpec {
    domains: Vec<Domain>,
}

#[allow(dead_code)]
#[derive(serde::Deserialize, Clone)]
struct Domain {
    domain: String,
    #[serde(default)]
    types: Vec<TypeDef>,
    #[serde(default)]
    commands: Vec<Command>,
    #[serde(default)]
    events: Vec<Event>,
}

#[allow(dead_code)]
#[derive(serde::Deserialize, Clone)]
struct TypeDef {
    id: String,
    #[serde(rename = "type", default)]
    type_kind: String,
    #[serde(default)]
    properties: Vec<Property>,
    #[serde(rename = "enum", default)]
    enum_values: Vec<String>,
    #[serde(default)]
    description: Option<String>,
}

#[allow(dead_code)]
#[derive(serde::Deserialize, Clone)]
struct Command {
    name: String,
    #[serde(default)]
    parameters: Vec<Property>,
    #[serde(default)]
    returns: Vec<Property>,
    #[serde(default)]
    description: Option<String>,
}

#[allow(dead_code)]
#[derive(serde::Deserialize, Clone)]
struct Event {
    name: String,
    #[serde(default)]
    parameters: Vec<Property>,
    #[serde(default)]
    description: Option<String>,
}

#[allow(dead_code)]
#[derive(serde::Deserialize, Clone)]
struct Property {
    name: String,
    #[serde(rename = "type", default)]
    type_kind: Option<String>,
    #[serde(rename = "$ref", default)]
    ref_type: Option<String>,
    #[serde(default)]
    optional: bool,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    items: Option<Box<ItemType>>,
    #[serde(rename = "enum", default)]
    enum_values: Vec<String>,
}

#[allow(dead_code)]
#[derive(serde::Deserialize, Clone)]
struct ItemType {
    #[serde(rename = "type", default)]
    type_kind: Option<String>,
    #[serde(rename = "$ref", default)]
    ref_type: Option<String>,
}

fn to_pascal_case(s: &str) -> String {
    let mut result = String::new();
    let mut capitalize = true;
    for c in s.chars() {
        if c == '_' || c == '-' || c == '.' {
            capitalize = true;
        } else if capitalize {
            result.push(c.to_ascii_uppercase());
            capitalize = false;
        } else {
            result.push(c);
        }
    }
    result
}

fn to_snake_case(s: &str) -> String {
    let mut result = String::new();
    let chars: Vec<char> = s.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c.is_uppercase() && i > 0 {
            // Only insert underscore at transitions from lowercase to uppercase,
            // or when an uppercase sequence ends (e.g. "DOM" -> "dom", not "d_o_m")
            let prev_upper = chars[i - 1].is_uppercase();
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_lowercase());
            if !prev_upper || next_lower {
                result.push('_');
            }
        }
        result.push(c.to_ascii_lowercase());
    }
    result
}

/// Resolve a $ref type reference. Cross-domain refs like "Page.FrameId" become
/// `super::cdp_page::FrameId`. Same-domain refs are used directly.
fn resolve_ref(
    r: &str,
    current_domain: &str,
    domain_types: &std::collections::HashMap<String, HashSet<String>>,
) -> String {
    let parts: Vec<&str> = r.split('.').collect();
    if parts.len() == 2 {
        let ref_domain = parts[0];
        let ref_type = parts[1];
        if ref_domain == current_domain {
            to_pascal_case(ref_type)
        } else {
            // Check if this type actually exists in the referenced domain
            if domain_types
                .get(ref_domain)
                .is_some_and(|t| t.contains(ref_type))
            {
                format!(
                    "super::cdp_{}::{}",
                    to_snake_case(ref_domain),
                    to_pascal_case(ref_type)
                )
            } else {
                // Fall back to serde_json::Value for unknown cross-domain refs
                "serde_json::Value".to_string()
            }
        }
    } else {
        to_pascal_case(r)
    }
}

fn map_type_in_domain(
    prop: &Property,
    current_domain: &str,
    domain_types: &std::collections::HashMap<String, HashSet<String>>,
) -> String {
    if let Some(ref r) = prop.ref_type {
        let type_name = resolve_ref(r, current_domain, domain_types);
        if prop.optional {
            format!("Option<{}>", type_name)
        } else {
            type_name
        }
    } else if let Some(ref t) = prop.type_kind {
        let base = match t.as_str() {
            "string" => "String".to_string(),
            "integer" => "i64".to_string(),
            "number" => "f64".to_string(),
            "boolean" => "bool".to_string(),
            "object" => "serde_json::Value".to_string(),
            "any" => "serde_json::Value".to_string(),
            "array" => {
                if let Some(ref items) = prop.items {
                    let inner = if let Some(ref r) = items.ref_type {
                        resolve_ref(r, current_domain, domain_types)
                    } else {
                        match items.type_kind.as_deref().unwrap_or("any") {
                            "string" => "String".to_string(),
                            "integer" => "i64".to_string(),
                            "number" => "f64".to_string(),
                            "boolean" => "bool".to_string(),
                            _ => "serde_json::Value".to_string(),
                        }
                    };
                    format!("Vec<{}>", inner)
                } else {
                    "Vec<serde_json::Value>".to_string()
                }
            }
            _ => "serde_json::Value".to_string(),
        };
        if prop.optional {
            format!("Option<{}>", base)
        } else {
            base
        }
    } else if prop.optional {
        "Option<serde_json::Value>".to_string()
    } else {
        "serde_json::Value".to_string()
    }
}

fn is_rust_keyword(s: &str) -> bool {
    matches!(
        s,
        "type"
            | "self"
            | "Self"
            | "super"
            | "move"
            | "ref"
            | "fn"
            | "mod"
            | "use"
            | "pub"
            | "let"
            | "mut"
            | "const"
            | "static"
            | "if"
            | "else"
            | "for"
            | "while"
            | "loop"
            | "match"
            | "return"
            | "break"
            | "continue"
            | "as"
            | "in"
            | "impl"
            | "trait"
            | "struct"
            | "enum"
            | "where"
            | "async"
            | "await"
            | "dyn"
            | "box"
            | "yield"
            | "override"
            | "crate"
            | "extern"
    )
}

fn generate_domain(
    domain: &Domain,
    domain_types: &std::collections::HashMap<String, HashSet<String>>,
    recursive_fields: &HashSet<(&str, &str, &str)>,
    output: &mut String,
) {
    let mod_name = to_snake_case(&domain.domain);
    output.push_str(&format!(
        "#[allow(dead_code, non_snake_case, non_camel_case_types, clippy::enum_variant_names)]\npub mod cdp_{} {{\n",
        mod_name
    ));
    output.push_str("    use super::*;\n\n");

    for type_def in &domain.types {
        if !type_def.enum_values.is_empty() {
            // Deduplicate enum variants (some CDP enums have duplicated PascalCase forms)
            let mut seen_variants = HashSet::new();
            output.push_str("    #[derive(Debug, Clone, Serialize, Deserialize)]\n");
            output.push_str(&format!("    pub enum {} {{\n", type_def.id));
            for val in &type_def.enum_values {
                let mut variant = to_pascal_case(val);
                if variant == "Self" {
                    variant = "SelfValue".to_string();
                }
                if variant.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                    variant = format!("V{}", variant);
                }
                if seen_variants.insert(variant.clone()) {
                    output.push_str(&format!(
                        "        #[serde(rename = \"{}\")]\n        {},\n",
                        val, variant
                    ));
                }
            }
            output.push_str("    }\n\n");
        } else if type_def.type_kind == "object" && !type_def.properties.is_empty() {
            output.push_str(
                "    #[derive(Debug, Clone, Serialize, Deserialize)]\n    #[serde(rename_all = \"camelCase\")]\n",
            );
            output.push_str(&format!("    pub struct {} {{\n", type_def.id));
            for prop in &type_def.properties {
                let field_name = to_snake_case(&prop.name);
                let field_name = if is_rust_keyword(&field_name) {
                    format!("r#{}", field_name)
                } else {
                    field_name
                };
                let mut rust_type = map_type_in_domain(prop, &domain.domain, domain_types);

                // Wrap recursive fields in Box
                if recursive_fields.contains(&(
                    domain.domain.as_str(),
                    type_def.id.as_str(),
                    prop.name.as_str(),
                )) {
                    if rust_type.starts_with("Option<") {
                        let inner = &rust_type[7..rust_type.len() - 1];
                        rust_type = format!("Option<Box<{}>>", inner);
                    } else {
                        rust_type = format!("Box<{}>", rust_type);
                    }
                }

                if prop.optional {
                    output
                        .push_str("        #[serde(skip_serializing_if = \"Option::is_none\")]\n");
                }
                output.push_str(&format!("        pub {}: {},\n", field_name, rust_type));
            }
            output.push_str("    }\n\n");
        } else if type_def.type_kind == "object" && type_def.properties.is_empty() {
            output.push_str(&format!(
                "    pub type {} = serde_json::Value;\n\n",
                type_def.id
            ));
        } else if type_def.type_kind == "array" {
            output.push_str(&format!(
                "    pub type {} = Vec<serde_json::Value>;\n\n",
                type_def.id
            ));
        } else if type_def.type_kind == "string" && type_def.enum_values.is_empty() {
            output.push_str(&format!("    pub type {} = String;\n\n", type_def.id));
        } else if type_def.type_kind == "integer" {
            output.push_str(&format!("    pub type {} = i64;\n\n", type_def.id));
        } else if type_def.type_kind == "number" {
            output.push_str(&format!("    pub type {} = f64;\n\n", type_def.id));
        }
    }

    for cmd in &domain.commands {
        let pascal_name = to_pascal_case(&cmd.name);

        if !cmd.parameters.is_empty() {
            output.push_str(
                "    #[derive(Debug, Clone, Serialize, Deserialize)]\n    #[serde(rename_all = \"camelCase\")]\n",
            );
            output.push_str(&format!("    pub struct {}Params {{\n", pascal_name));
            for param in &cmd.parameters {
                let field_name = to_snake_case(&param.name);
                let field_name = if is_rust_keyword(&field_name) {
                    format!("r#{}", field_name)
                } else {
                    field_name
                };
                let rust_type = map_type_in_domain(param, &domain.domain, domain_types);
                if param.optional {
                    output
                        .push_str("        #[serde(skip_serializing_if = \"Option::is_none\")]\n");
                }
                output.push_str(&format!("        pub {}: {},\n", field_name, rust_type));
            }
            output.push_str("    }\n\n");
        }

        if !cmd.returns.is_empty() {
            output.push_str(
                "    #[derive(Debug, Clone, Serialize, Deserialize)]\n    #[serde(rename_all = \"camelCase\")]\n",
            );
            output.push_str(&format!("    pub struct {}Result {{\n", pascal_name));
            for ret in &cmd.returns {
                let field_name = to_snake_case(&ret.name);
                let field_name = if is_rust_keyword(&field_name) {
                    format!("r#{}", field_name)
                } else {
                    field_name
                };
                let rust_type = map_type_in_domain(ret, &domain.domain, domain_types);
                if ret.optional {
                    output
                        .push_str("        #[serde(skip_serializing_if = \"Option::is_none\")]\n");
                }
                output.push_str(&format!("        pub {}: {},\n", field_name, rust_type));
            }
            output.push_str("    }\n\n");
        }
    }

    for event in &domain.events {
        if !event.parameters.is_empty() {
            let pascal_name = to_pascal_case(&event.name);
            output.push_str(
                "    #[derive(Debug, Clone, Serialize, Deserialize)]\n    #[serde(rename_all = \"camelCase\")]\n",
            );
            output.push_str(&format!("    pub struct {}Event {{\n", pascal_name));
            for param in &event.parameters {
                let field_name = to_snake_case(&param.name);
                let field_name = if is_rust_keyword(&field_name) {
                    format!("r#{}", field_name)
                } else {
                    field_name
                };
                let rust_type = map_type_in_domain(param, &domain.domain, domain_types);
                if param.optional {
                    output
                        .push_str("        #[serde(skip_serializing_if = \"Option::is_none\")]\n");
                }
                output.push_str(&format!("        pub {}: {},\n", field_name, rust_type));
            }
            output.push_str("    }\n\n");
        }
    }

    output.push_str("}\n\n");
}
