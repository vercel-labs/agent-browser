//! Skills over MCP (`io.modelcontextprotocol/skills`, SEP-2640) and base
//! MCP Resources implementation backed by the shared skill catalog in `skills.rs`.

use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

use crate::mcp_transport::{ProtocolEra, ProtocolError, LIST_TTL_MS};
use crate::skills::{load_skill_packages, SkillFileEntry, SkillPackage};

pub(crate) const SKILLS_EXTENSION_ID: &str = "io.modelcontextprotocol/skills";
const PAGE_SIZE: usize = 64;
const MAX_SKILL_RESOURCES: usize = 512;
const MAX_SKILL_TOTAL_BYTES: usize = 16 * 1024 * 1024;

pub(crate) fn skills_extension_capability() -> Value {
    json!({
        "directoryRead": true
    })
}

pub(crate) fn list_skills(params: Option<&Value>) -> Result<Value, ProtocolError> {
    let packages = load_skill_packages();
    list_skills_from_packages(params, &packages)
}

pub(crate) fn get_skill(params: Option<&Value>) -> Result<Value, ProtocolError> {
    let packages = load_skill_packages();
    get_skill_from_packages(params, &packages)
}

pub(crate) fn list_resources(
    params: Option<&Value>,
    era: ProtocolEra,
) -> Result<Value, ProtocolError> {
    let packages = load_skill_packages();
    list_resources_from_packages(params, &packages, era)
}

pub(crate) fn list_resource_templates(era: ProtocolEra) -> Value {
    let mut result = json!({
        "resourceTemplates": [],
    });
    if era == ProtocolEra::Modern {
        result["ttlMs"] = json!(LIST_TTL_MS);
        result["cacheScope"] = json!("public");
    }
    result
}

pub(crate) fn read_resource(
    params: Option<&Value>,
    era: ProtocolEra,
) -> Result<Value, ProtocolError> {
    let packages = load_skill_packages();
    read_resource_from_packages(params, &packages, era)
}

pub(crate) fn read_resource_directory(params: Option<&Value>) -> Result<Value, ProtocolError> {
    let packages = load_skill_packages();
    read_resource_directory_from_packages(params, &packages)
}

fn list_skills_from_packages(
    params: Option<&Value>,
    packages: &[SkillPackage],
) -> Result<Value, ProtocolError> {
    let visible: Vec<Value> = packages
        .iter()
        .filter(|pkg| !pkg.hidden)
        .filter_map(skill_entry_value)
        .collect();

    let start = parse_cursor(params, visible.len(), "skills/list")?;
    let end = (start + PAGE_SIZE).min(visible.len());

    let mut result = json!({
        "resultType": "complete",
        "skills": visible[start..end].to_vec(),
        "ttlMs": LIST_TTL_MS,
        "cacheScope": "public",
    });
    if end < visible.len() {
        result["nextCursor"] = json!(end.to_string());
    }
    Ok(result)
}

fn get_skill_from_packages(
    params: Option<&Value>,
    packages: &[SkillPackage],
) -> Result<Value, ProtocolError> {
    let uri = params
        .and_then(|p| p.get("uri"))
        .and_then(Value::as_str)
        .ok_or_else(|| ProtocolError::invalid_params("skills/get requires string params.uri"))?;

    let pkg = packages
        .iter()
        .find(|pkg| skill_md_uri(&pkg.name) == uri)
        .ok_or_else(|| ProtocolError::invalid_params(format!("Skill not found: {}", uri)))?;

    let entry = skill_entry_value(pkg)
        .ok_or_else(|| ProtocolError::invalid_params(format!("Skill exceeds limits: {}", uri)))?;

    Ok(json!({
        "resultType": "complete",
        "skill": entry,
        "ttlMs": LIST_TTL_MS,
        "cacheScope": "public",
    }))
}

fn list_resources_from_packages(
    params: Option<&Value>,
    packages: &[SkillPackage],
    era: ProtocolEra,
) -> Result<Value, ProtocolError> {
    let mut items = Vec::new();
    for pkg in packages.iter().filter(|pkg| !pkg.hidden) {
        for file in &pkg.files {
            let uri = skill_file_uri(&pkg.name, &file.rel_path);
            let mut item = json!({
                "uri": uri,
                "name": if file.rel_path == "SKILL.md" {
                    pkg.name.clone()
                } else {
                    file_basename(&file.rel_path).to_string()
                },
                "mimeType": mime_type_for_path(&file.rel_path),
                "size": file.bytes.len(),
            });
            if file.rel_path == "SKILL.md" && !pkg.description.is_empty() {
                item["description"] = json!(pkg.description);
            }
            items.push(item);
        }
    }

    let start = parse_cursor(params, items.len(), "resources/list")?;
    let end = (start + PAGE_SIZE).min(items.len());
    let mut result = json!({
        "resources": items[start..end].to_vec(),
    });
    if end < items.len() {
        result["nextCursor"] = json!(end.to_string());
    }
    if era == ProtocolEra::Modern {
        result["ttlMs"] = json!(LIST_TTL_MS);
        result["cacheScope"] = json!("public");
    }
    Ok(result)
}

fn read_resource_from_packages(
    params: Option<&Value>,
    packages: &[SkillPackage],
    era: ProtocolEra,
) -> Result<Value, ProtocolError> {
    let uri = params
        .and_then(|p| p.get("uri"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ProtocolError::invalid_params("resources/read requires string params.uri")
        })?;

    let (skill_name, rel_path) = parse_skill_file_uri(uri)
        .ok_or_else(|| ProtocolError::invalid_params(format!("Resource not found: {}", uri)))?;

    let pkg = packages
        .iter()
        .find(|pkg| pkg.name == skill_name)
        .ok_or_else(|| ProtocolError::invalid_params(format!("Resource not found: {}", uri)))?;

    let file = pkg
        .files
        .iter()
        .find(|f| f.rel_path == rel_path)
        .ok_or_else(|| ProtocolError::invalid_params(format!("Resource not found: {}", uri)))?;

    let content = resource_content_entry(uri, file);
    let mut result = json!({
        "contents": [content],
    });
    if era == ProtocolEra::Modern {
        result["ttlMs"] = json!(LIST_TTL_MS);
        result["cacheScope"] = json!("public");
    }
    Ok(result)
}

fn read_resource_directory_from_packages(
    params: Option<&Value>,
    packages: &[SkillPackage],
) -> Result<Value, ProtocolError> {
    let uri = params
        .and_then(|p| p.get("uri"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ProtocolError::invalid_params("resources/directory/read requires string params.uri")
        })?;

    if uri.ends_with('/') {
        return Err(ProtocolError::invalid_params(format!(
            "Directory URI must not have a trailing slash: {}",
            uri
        )));
    }

    let (skill_name, dir_prefix) = parse_skill_directory_uri(uri)
        .ok_or_else(|| ProtocolError::invalid_params(format!("Directory not found: {}", uri)))?;

    let pkg = packages
        .iter()
        .find(|pkg| pkg.name == skill_name)
        .ok_or_else(|| ProtocolError::invalid_params(format!("Directory not found: {}", uri)))?;

    let mut children: BTreeMap<String, Value> = BTreeMap::new();
    for file in &pkg.files {
        let remainder = if dir_prefix.is_empty() {
            file.rel_path.as_str()
        } else {
            let prefix_with_slash = format!("{}/", dir_prefix);
            let Some(rest) = file.rel_path.strip_prefix(&prefix_with_slash) else {
                continue;
            };
            rest
        };
        if let Some((subdir, _)) = remainder.split_once('/') {
            let child_uri = format!("{}/{}", uri, subdir);
            children.entry(child_uri.clone()).or_insert_with(|| {
                json!({
                    "uri": child_uri,
                    "name": subdir,
                    "mimeType": "inode/directory",
                })
            });
        } else {
            let child_uri = format!("{}/{}", uri, remainder);
            children.insert(
                child_uri.clone(),
                json!({
                    "uri": child_uri,
                    "name": remainder,
                    "mimeType": mime_type_for_path(&file.rel_path),
                    "size": file.bytes.len(),
                }),
            );
        }
    }

    if children.is_empty() {
        return Err(ProtocolError::invalid_params(format!(
            "Directory not found: {}",
            uri
        )));
    }

    let all: Vec<Value> = children.into_values().collect();
    let start = parse_cursor(params, all.len(), "resources/directory/read")?;
    let end = (start + PAGE_SIZE).min(all.len());

    let mut result = json!({
        "resultType": "complete",
        "resources": all[start..end].to_vec(),
        "ttlMs": LIST_TTL_MS,
        "cacheScope": "public",
    });
    if end < all.len() {
        result["nextCursor"] = json!(end.to_string());
    }
    Ok(result)
}

fn skill_entry_value(pkg: &SkillPackage) -> Option<Value> {
    if pkg.files.len() > MAX_SKILL_RESOURCES {
        return None;
    }
    let total_bytes: usize = pkg.files.iter().map(|f| f.bytes.len()).sum();
    if total_bytes > MAX_SKILL_TOTAL_BYTES {
        return None;
    }

    let resources: Vec<Value> = pkg
        .files
        .iter()
        .map(|file| {
            json!({
                "uri": skill_file_uri(&pkg.name, &file.rel_path),
                "digest": sha256_digest(&file.bytes),
                "size": file.bytes.len(),
            })
        })
        .collect();

    Some(json!({
        "uri": skill_md_uri(&pkg.name),
        "frontmatter": Value::Object(pkg.frontmatter.clone()),
        "resources": resources,
    }))
}

fn resource_content_entry(uri: &str, file: &SkillFileEntry) -> Value {
    let mime = mime_type_for_path(&file.rel_path);
    match std::str::from_utf8(&file.bytes) {
        Ok(text) => json!({
            "uri": uri,
            "mimeType": mime,
            "text": text,
        }),
        Err(_) => json!({
            "uri": uri,
            "mimeType": mime,
            "blob": STANDARD.encode(&file.bytes),
        }),
    }
}

fn skill_md_uri(name: &str) -> String {
    format!("skill://{}/SKILL.md", name)
}

fn skill_file_uri(name: &str, rel_path: &str) -> String {
    format!("skill://{}/{}", name, rel_path)
}

fn parse_skill_file_uri(uri: &str) -> Option<(&str, &str)> {
    let rest = uri.strip_prefix("skill://")?;
    if rest.contains('\\') {
        return None;
    }
    let (name, rel_path) = rest.split_once('/')?;
    if name.is_empty() || rel_path.is_empty() || !is_safe_rel_path(rel_path) {
        return None;
    }
    Some((name, rel_path))
}

fn parse_skill_directory_uri(uri: &str) -> Option<(&str, &str)> {
    let rest = uri.strip_prefix("skill://")?;
    if rest.is_empty() || rest.contains('\\') {
        return None;
    }
    match rest.split_once('/') {
        Some((name, dir_prefix)) => {
            if name.is_empty() || dir_prefix.is_empty() || !is_safe_rel_path(dir_prefix) {
                return None;
            }
            Some((name, dir_prefix))
        }
        None => {
            if rest == "." || rest == ".." {
                return None;
            }
            Some((rest, ""))
        }
    }
}

fn is_safe_rel_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.ends_with('/')
        && path
            .split('/')
            .all(|seg| !seg.is_empty() && seg != "." && seg != "..")
}

fn sha256_digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

fn file_basename(rel_path: &str) -> &str {
    rel_path.rsplit('/').next().unwrap_or(rel_path)
}

fn mime_type_for_path(rel_path: &str) -> &'static str {
    let lower = rel_path.to_ascii_lowercase();
    if lower.ends_with(".md") || lower.ends_with(".mdx") {
        "text/markdown"
    } else if lower.ends_with(".sh") {
        "text/x-shellscript"
    } else if lower.ends_with(".json") {
        "application/json"
    } else if lower.ends_with(".js") || lower.ends_with(".mjs") {
        "text/javascript"
    } else if lower.ends_with(".ts") {
        "text/typescript"
    } else if lower.ends_with(".yaml") || lower.ends_with(".yml") {
        "application/yaml"
    } else {
        "text/plain"
    }
}

fn parse_cursor(
    params: Option<&Value>,
    total: usize,
    method: &str,
) -> Result<usize, ProtocolError> {
    let Some(cursor) = params.and_then(|p| p.get("cursor")) else {
        return Ok(0);
    };
    let cursor = cursor.as_str().ok_or_else(|| {
        ProtocolError::invalid_params(format!("{} cursor must be a string", method))
    })?;
    let index = cursor
        .parse::<usize>()
        .map_err(|_| ProtocolError::invalid_params(format!("Invalid {} cursor", method)))?;
    if index > total {
        return Err(ProtocolError::invalid_params(format!(
            "Invalid {} cursor",
            method
        )));
    }
    Ok(index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::{
        load_embedded_skill_packages, load_skill_packages_from_dirs, parse_frontmatter_json,
    };
    use std::path::PathBuf;

    fn repo_skill_dirs() -> Vec<PathBuf> {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
        vec![root.join("skills"), root.join("skill-data")]
    }

    #[test]
    fn skills_list_and_embedded_fallback_match_repo_files_byte_for_byte() {
        let disk_packages = load_skill_packages_from_dirs(&repo_skill_dirs());
        let embedded_packages = load_embedded_skill_packages();
        assert_eq!(disk_packages.len(), embedded_packages.len());
        assert!(disk_packages.len() >= 10);

        let listed = list_skills_from_packages(None, &disk_packages).unwrap();
        assert_eq!(listed["resultType"], "complete");
        assert_eq!(listed["cacheScope"], "public");
        assert_eq!(listed["ttlMs"], LIST_TTL_MS);

        let skills = listed["skills"].as_array().unwrap();
        // Hidden discovery stub is omitted from skills/list.
        assert!(skills
            .iter()
            .all(|s| s["uri"] != "skill://agent-browser/SKILL.md"));
        let core = skills
            .iter()
            .find(|s| s["uri"] == "skill://core/SKILL.md")
            .expect("core skill present");
        assert_eq!(core["frontmatter"]["name"], "core");
        assert!(core["frontmatter"]["description"]
            .as_str()
            .unwrap()
            .contains("Core agent-browser"));
        assert!(core["frontmatter"]["allowed-tools"].is_string());

        // Verify every manifest entry digest, byte size, and frontmatter against resources/read.
        for skill in skills {
            let uri = skill["uri"].as_str().unwrap();
            let resources = skill["resources"].as_array().unwrap();
            assert!(
                resources.iter().any(|r| r["uri"] == uri),
                "manifest must include SKILL.md for {}",
                uri
            );
            for res in resources {
                let res_uri = res["uri"].as_str().unwrap();
                let read = read_resource_from_packages(
                    Some(&json!({ "uri": res_uri })),
                    &disk_packages,
                    ProtocolEra::Modern,
                )
                .unwrap();
                let text = read["contents"][0]["text"].as_str().unwrap();
                assert_eq!(res["size"].as_u64().unwrap() as usize, text.len());
                assert_eq!(
                    res["digest"].as_str().unwrap(),
                    sha256_digest(text.as_bytes())
                );
                if res_uri == uri {
                    let reparsed = Value::Object(parse_frontmatter_json(text).unwrap());
                    assert_eq!(skill["frontmatter"], reparsed);
                }
            }
        }

        // Embedded fallback produces identical manifest for core.
        let embedded_core = get_skill_from_packages(
            Some(&json!({ "uri": "skill://core/SKILL.md" })),
            &embedded_packages,
        )
        .unwrap();
        let disk_core = get_skill_from_packages(
            Some(&json!({ "uri": "skill://core/SKILL.md" })),
            &disk_packages,
        )
        .unwrap();
        assert_eq!(embedded_core["skill"], disk_core["skill"]);
    }

    #[test]
    fn skills_get_resolves_hidden_stub_and_rejects_unknown_uri() {
        let packages = load_skill_packages_from_dirs(&repo_skill_dirs());
        let stub = get_skill_from_packages(
            Some(&json!({ "uri": "skill://agent-browser/SKILL.md" })),
            &packages,
        )
        .unwrap();
        assert_eq!(stub["resultType"], "complete");
        assert_eq!(stub["skill"]["frontmatter"]["name"], "agent-browser");
        assert_eq!(stub["skill"]["frontmatter"]["hidden"], true);

        let err = get_skill_from_packages(
            Some(&json!({ "uri": "skill://nonexistent/SKILL.md" })),
            &packages,
        )
        .unwrap_err();
        assert_eq!(err.code, -32602);
    }

    #[test]
    fn directory_read_lists_direct_children_and_rejects_invalid_uris() {
        let packages = load_skill_packages_from_dirs(&repo_skill_dirs());
        let root = read_resource_directory_from_packages(
            Some(&json!({ "uri": "skill://core" })),
            &packages,
        )
        .unwrap();
        assert_eq!(root["resultType"], "complete");
        let children = root["resources"].as_array().unwrap();
        assert!(children
            .iter()
            .any(|c| { c["uri"] == "skill://core/SKILL.md" && c["mimeType"] == "text/markdown" }));
        assert!(children.iter().any(|c| {
            c["uri"] == "skill://core/references" && c["mimeType"] == "inode/directory"
        }));
        assert!(children.iter().any(|c| {
            c["uri"] == "skill://core/templates" && c["mimeType"] == "inode/directory"
        }));
        // Direct children only: nested files must not appear in root directory listing.
        assert!(children
            .iter()
            .all(|c| c["uri"] != "skill://core/references/commands.md"));

        let refs = read_resource_directory_from_packages(
            Some(&json!({ "uri": "skill://core/references" })),
            &packages,
        )
        .unwrap();
        assert!(refs["resources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["uri"] == "skill://core/references/commands.md"));

        // Trailing slash rejected.
        assert_eq!(
            read_resource_directory_from_packages(
                Some(&json!({ "uri": "skill://core/" })),
                &packages
            )
            .unwrap_err()
            .code,
            -32602
        );
        // File URI rejected.
        assert_eq!(
            read_resource_directory_from_packages(
                Some(&json!({ "uri": "skill://core/SKILL.md" })),
                &packages
            )
            .unwrap_err()
            .code,
            -32602
        );
        // Traversal rejected on resources/read.
        assert_eq!(
            read_resource_from_packages(
                Some(&json!({ "uri": "skill://core/../dogfood/SKILL.md" })),
                &packages,
                ProtocolEra::Modern
            )
            .unwrap_err()
            .code,
            -32602
        );
    }

    #[cfg(unix)]
    #[test]
    fn skill_loader_rejects_symlinks_pointing_outside_package_root() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().unwrap();
        let outside_dir = tmp.path().join("outside");
        let outside_secret = outside_dir.join("secret.txt");
        let outside_skill_dir = outside_dir.join("external-skill");
        std::fs::create_dir_all(&outside_skill_dir).unwrap();
        std::fs::write(&outside_secret, "TOP_SECRET_VALUE").unwrap();
        std::fs::write(
            outside_skill_dir.join("SKILL.md"),
            "---\nname: escaped-dir\ndescription: outside skill\n---\nsecret\n",
        )
        .unwrap();

        let skills_root = tmp.path().join("skills-root");
        let safe_pkg = skills_root.join("demo");
        let refs_dir = safe_pkg.join("references");
        std::fs::create_dir_all(&refs_dir).unwrap();
        std::fs::write(
            safe_pkg.join("SKILL.md"),
            "---\nname: demo\ndescription: demo skill\n---\n# Demo\n",
        )
        .unwrap();
        std::fs::write(refs_dir.join("ok.md"), "safe content\n").unwrap();

        // Symlink to a file outside the skill directory.
        symlink(&outside_secret, refs_dir.join("leak.md")).unwrap();
        // Symlink to a directory outside the skill directory.
        symlink(&outside_dir, safe_pkg.join("linked-outside")).unwrap();
        // Symlink within the same skill package stays allowed.
        symlink(refs_dir.join("ok.md"), refs_dir.join("alias.md")).unwrap();
        // Top-level symlinked skill directory pointing outside skills_root is rejected.
        symlink(&outside_skill_dir, skills_root.join("escaped-dir")).unwrap();
        // Skill whose SKILL.md is a symlink outside its package root is rejected.
        let bad_md_pkg = skills_root.join("bad-md");
        std::fs::create_dir_all(&bad_md_pkg).unwrap();
        symlink(
            outside_skill_dir.join("SKILL.md"),
            bad_md_pkg.join("SKILL.md"),
        )
        .unwrap();

        let packages = load_skill_packages_from_dirs(&[skills_root]);
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].name, "demo");

        let listed = list_skills_from_packages(None, &packages).unwrap();
        let demo = &listed["skills"][0];
        let uris: Vec<&str> = demo["resources"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|r| r["uri"].as_str())
            .collect();
        assert!(uris.contains(&"skill://demo/SKILL.md"));
        assert!(uris.contains(&"skill://demo/references/ok.md"));
        assert!(uris.contains(&"skill://demo/references/alias.md"));
        assert!(!uris.iter().any(|u| u.contains("leak.md")));
        assert!(!uris.iter().any(|u| u.contains("linked-outside")));
        assert!(!uris.iter().any(|u| u.contains("secret.txt")));

        // Direct resources/read against the symlinked outside file fails with -32602.
        let err = read_resource_from_packages(
            Some(&json!({ "uri": "skill://demo/references/leak.md" })),
            &packages,
            ProtocolEra::Modern,
        )
        .unwrap_err();
        assert_eq!(err.code, -32602);
    }
}
