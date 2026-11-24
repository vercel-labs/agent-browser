use rust_embed::Embed;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashSet};
use std::env;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::exit;

use crate::color;

#[derive(Embed)]
#[folder = "../skill-data/"]
struct EmbeddedSkillData;

#[derive(Embed)]
#[folder = "../skills/"]
struct EmbeddedDiscoverySkills;

/// A single file inside a skill package, preserving exact raw bytes.
#[derive(Debug, Clone)]
pub(crate) struct SkillFileEntry {
    /// Forward-slash relative path inside the skill directory (e.g. `SKILL.md`, `references/commands.md`).
    pub(crate) rel_path: String,
    /// Exact raw bytes served for this file.
    pub(crate) bytes: Vec<u8>,
}

/// Complete skill package shared by the CLI and `agent-browser mcp`.
#[derive(Debug, Clone)]
pub(crate) struct SkillPackage {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) hidden: bool,
    pub(crate) frontmatter: Map<String, Value>,
    pub(crate) files: Vec<SkillFileEntry>,
}

struct SkillInfo {
    name: String,
    description: String,
    dir: PathBuf,
    /// When true, the skill is omitted from `skills list` and `skills get --all`
    /// but can still be fetched by name via `skills get <name>`. Used for
    /// bootstrap stubs that exist for external tooling (e.g. `npx skills add`)
    /// but aren't the intended entry point for agents already inside the CLI.
    hidden: bool,
}

/// Skill content is split across two directories:
///
/// - `skills/` — discovery stubs (picked up by `npx skills add`). Carry
///   `hidden: true` so they don't show up in `skills list` or `skills get
///   --all` inside the CLI, since they exist only to redirect external
///   agents to `skills get core`.
/// - `skill-data/` — runtime skill content served by the CLI (`core`,
///   `electron`, `slack`, `dogfood`, etc.).
///
/// Both are shipped in the npm package and searched by `discover_skills`.
const SKILL_DIRS: &[&str] = &["skills", "skill-data"];

/// Locate the package root that contains the skill directories.
///
/// Resolution order:
/// 1. AGENT_BROWSER_SKILLS_DIR env var (points directly at a single directory)
/// 2. ../  relative to the executable (npm installs: binary is in bin/)
/// 3. Walk up from the executable to find a project root with skill-data/core/SKILL.md
///    (dev builds where binary is in target/debug/ or target/release/)
fn find_package_root() -> Option<PathBuf> {
    if let Ok(exe) = env::current_exe() {
        let exe = exe.canonicalize().unwrap_or(exe);
        if let Some(parent) = exe.parent() {
            // npm install layout: bin/agent-browser-* -> ../
            let candidate = parent.join("..");
            if candidate.join("skill-data/core/SKILL.md").is_file() {
                return Some(candidate.canonicalize().unwrap_or(candidate));
            }

            // dev build layout: walk up from target/debug/ or target/release/
            let mut dir = parent;
            loop {
                if dir.join("skill-data/core/SKILL.md").is_file() {
                    return Some(dir.to_path_buf());
                }
                match dir.parent() {
                    Some(p) => dir = p,
                    None => break,
                }
            }
        }
    }

    None
}

/// Collect skill directories, falling back to `~/.agent-browser` when package discovery fails.
fn find_skills_dirs() -> Vec<PathBuf> {
    // Env var override: single directory, used as-is
    if let Ok(dir) = env::var("AGENT_BROWSER_SKILLS_DIR") {
        let p = PathBuf::from(dir);
        if p.is_dir() {
            return vec![p];
        }
    }

    let Some(root) = find_package_root()
        .or_else(|| std::env::home_dir().map(|home| home.join(".agent-browser")))
    else {
        return vec![];
    };

    SKILL_DIRS
        .iter()
        .map(|d| root.join(d))
        .filter(|p| p.is_dir())
        .collect()
}

/// Parse YAML frontmatter from a SKILL.md file into a verbatim JSON object.
pub(crate) fn parse_frontmatter_json(content: &str) -> Option<Map<String, Value>> {
    let content = content.trim_start();
    if !content.starts_with("---") {
        return None;
    }
    let after_opening = &content[3..];
    let after_opening = after_opening
        .strip_prefix("\r\n")
        .or_else(|| after_opening.strip_prefix('\n'))?;
    let end = find_frontmatter_end(after_opening)?;
    let frontmatter = &after_opening[..end];
    let lines: Vec<&str> = frontmatter.lines().collect();
    let (map, _) = parse_yaml_mapping(&lines, 0, 0);
    Some(map)
}

fn find_frontmatter_end(text: &str) -> Option<usize> {
    let mut offset = 0;
    for segment in text.split_inclusive('\n') {
        let trimmed = segment.trim_end_matches(&['\r', '\n'][..]);
        if trimmed == "---" {
            return Some(offset);
        }
        offset += segment.len();
    }
    if text.trim_end_matches(&['\r', '\n'][..]) == "---" {
        return Some(0);
    }
    None
}

fn line_indent(line: &str) -> usize {
    line.bytes()
        .take_while(|b| *b == b' ' || *b == b'\t')
        .count()
}

fn parse_yaml_mapping(
    lines: &[&str],
    mut i: usize,
    base_indent: usize,
) -> (Map<String, Value>, usize) {
    let mut map = Map::new();
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            i += 1;
            continue;
        }
        let indent = line_indent(line);
        if indent < base_indent {
            break;
        }
        let stripped = &line[indent..];
        let Some((raw_key, rest)) = stripped.split_once(':') else {
            i += 1;
            continue;
        };
        let key = raw_key.trim().trim_matches(&['"', '\''][..]).to_string();
        if key.is_empty() {
            i += 1;
            continue;
        }
        let val_str = rest.trim();
        if val_str.is_empty() {
            // Nested block mapping or sequence on following indented lines.
            let mut next = i + 1;
            while next < lines.len()
                && (lines[next].trim().is_empty() || lines[next].trim().starts_with('#'))
            {
                next += 1;
            }
            if next < lines.len() && line_indent(lines[next]) > indent {
                let child_indent = line_indent(lines[next]);
                let child_first = lines[next][child_indent..].trim_start();
                if child_first.starts_with("- ") || child_first == "-" {
                    let (seq, after) = parse_yaml_sequence(lines, next, child_indent);
                    map.insert(key, Value::Array(seq));
                    i = after;
                } else {
                    let (child_map, after) = parse_yaml_mapping(lines, next, child_indent);
                    map.insert(key, Value::Object(child_map));
                    i = after;
                }
            } else {
                map.insert(key, Value::Null);
                i += 1;
            }
        } else if matches!(val_str, "|" | "|-" | "|+" | ">" | ">-" | ">+") {
            let folded = val_str.starts_with('>');
            let strip_final = val_str.ends_with('-');
            let mut block_lines: Vec<&str> = Vec::new();
            let mut block_indent: Option<usize> = None;
            i += 1;
            while i < lines.len() {
                let cur = lines[i];
                if cur.trim().is_empty() {
                    block_lines.push("");
                    i += 1;
                    continue;
                }
                let ind = line_indent(cur);
                if ind <= indent {
                    break;
                }
                let bi = *block_indent.get_or_insert(ind);
                let cut = bi.min(cur.len());
                block_lines.push(&cur[cut..]);
                i += 1;
            }
            while block_lines.last() == Some(&"") {
                block_lines.pop();
            }
            let mut joined = if folded {
                block_lines.join(" ")
            } else {
                block_lines.join("\n")
            };
            if !strip_final && !joined.is_empty() {
                joined.push('\n');
            }
            map.insert(key, Value::String(joined));
        } else {
            let mut scalar = val_str.to_string();
            let is_quoted = (scalar.starts_with('"') && scalar.ends_with('"') && scalar.len() >= 2)
                || (scalar.starts_with('\'') && scalar.ends_with('\'') && scalar.len() >= 2);
            if !is_quoted {
                while i + 1 < lines.len() {
                    let next_line = lines[i + 1];
                    if next_line.trim().is_empty() {
                        break;
                    }
                    let next_indent = line_indent(next_line);
                    if next_indent > indent && !next_line.trim_start().starts_with("- ") {
                        i += 1;
                        scalar.push(' ');
                        scalar.push_str(next_line.trim());
                    } else {
                        break;
                    }
                }
            }
            map.insert(key, parse_yaml_scalar(&scalar));
            i += 1;
        }
    }
    (map, i)
}

fn parse_yaml_sequence(lines: &[&str], mut i: usize, base_indent: usize) -> (Vec<Value>, usize) {
    let mut items = Vec::new();
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            i += 1;
            continue;
        }
        let indent = line_indent(line);
        if indent < base_indent {
            break;
        }
        let stripped = &line[indent..];
        let Some(rest) = stripped.strip_prefix('-') else {
            break;
        };
        let val = rest.trim();
        items.push(parse_yaml_scalar(val));
        i += 1;
    }
    (items, i)
}

fn parse_yaml_scalar(raw: &str) -> Value {
    let s = raw.trim();
    if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
        if let Ok(parsed) = serde_json::from_str::<String>(s) {
            return Value::String(parsed);
        }
        return Value::String(s[1..s.len() - 1].to_string());
    }
    if s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2 {
        return Value::String(s[1..s.len() - 1].replace("''", "'"));
    }
    if s.starts_with('[') && s.ends_with(']') {
        let inner = s[1..s.len() - 1].trim();
        if inner.is_empty() {
            return Value::Array(Vec::new());
        }
        let items = inner.split(',').map(parse_yaml_scalar).collect();
        return Value::Array(items);
    }
    match s {
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        "null" | "~" => Value::Null,
        _ => {
            if let Ok(n) = s.parse::<i64>() {
                Value::Number(n.into())
            } else {
                Value::String(s.to_string())
            }
        }
    }
}

/// Load skill packages from disk (`AGENT_BROWSER_SKILLS_DIR`, package root, or
/// `~/.agent-browser/`), falling back to compile-time embedded `skill-data/`
/// and `skills/` only when no filesystem skill directories exist and
/// `AGENT_BROWSER_SKILLS_DIR` is not explicitly set.
pub(crate) fn load_skill_packages() -> Vec<SkillPackage> {
    if let Ok(dir) = env::var("AGENT_BROWSER_SKILLS_DIR") {
        let p = PathBuf::from(dir);
        if p.is_dir() {
            return load_skill_packages_from_dirs(&[p]);
        }
        return Vec::new();
    }

    let dirs = find_skills_dirs();
    if !dirs.is_empty() {
        let loaded = load_skill_packages_from_dirs(&dirs);
        if !loaded.is_empty() {
            return loaded;
        }
    }

    load_embedded_skill_packages()
}

pub(crate) fn load_skill_packages_from_dirs(dirs: &[PathBuf]) -> Vec<SkillPackage> {
    let mut by_name: BTreeMap<String, SkillPackage> = BTreeMap::new();
    for skills_dir in dirs {
        let Ok(canon_skills_dir) = fs::canonicalize(skills_dir) else {
            continue;
        };
        let Ok(entries) = fs::read_dir(&canon_skills_dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(canon_pkg_root) = fs::canonicalize(&path) else {
                continue;
            };
            if !canon_pkg_root.is_dir() || !canon_pkg_root.starts_with(&canon_skills_dir) {
                continue;
            }
            let skill_md = path.join("SKILL.md");
            let Ok(canon_skill_md) = fs::canonicalize(&skill_md) else {
                continue;
            };
            if !canon_skill_md.is_file() || !canon_skill_md.starts_with(&canon_pkg_root) {
                continue;
            }
            let Ok(md_bytes) = fs::read(&canon_skill_md) else {
                continue;
            };
            let Ok(md_text) = std::str::from_utf8(&md_bytes) else {
                continue;
            };
            let Some(frontmatter) = parse_frontmatter_json(md_text) else {
                continue;
            };
            let Some(name) = frontmatter
                .get("name")
                .and_then(Value::as_str)
                .map(ToString::to_string)
            else {
                continue;
            };
            let description = frontmatter
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let hidden = frontmatter
                .get("hidden")
                .and_then(Value::as_bool)
                .unwrap_or(false);

            let mut files = Vec::new();
            let mut visited_dirs = HashSet::new();
            visited_dirs.insert(canon_pkg_root.clone());
            collect_dir_files_recursive(
                &canon_pkg_root,
                &path,
                &path,
                &mut visited_dirs,
                &mut files,
            );
            files.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
            by_name.insert(
                name.clone(),
                SkillPackage {
                    name,
                    description,
                    hidden,
                    frontmatter,
                    files,
                },
            );
        }
    }
    by_name.into_values().collect()
}

fn collect_dir_files_recursive(
    canon_root: &Path,
    logical_root: &Path,
    current: &Path,
    visited_dirs: &mut HashSet<PathBuf>,
    out: &mut Vec<SkillFileEntry>,
) {
    let Ok(entries) = fs::read_dir(current) else {
        return;
    };
    let mut items: Vec<_> = entries.flatten().collect();
    items.sort_by_key(|e| e.file_name());
    for entry in items {
        let path = entry.path();
        let Ok(canon_path) = fs::canonicalize(&path) else {
            continue;
        };
        // Reject symlinks or resolved paths that escape the skill package root.
        if !canon_path.starts_with(canon_root) {
            continue;
        }
        if canon_path.is_dir() {
            if visited_dirs.insert(canon_path) {
                collect_dir_files_recursive(canon_root, logical_root, &path, visited_dirs, out);
            }
        } else if canon_path.is_file() {
            let Ok(rel) = path.strip_prefix(logical_root) else {
                continue;
            };
            let mut parts = Vec::new();
            let mut valid = true;
            for comp in rel.components() {
                match comp {
                    Component::Normal(seg) => parts.push(seg.to_string_lossy().into_owned()),
                    _ => {
                        valid = false;
                        break;
                    }
                }
            }
            if !valid || parts.is_empty() {
                continue;
            }
            let rel_path = parts.join("/");
            if let Ok(bytes) = fs::read(&canon_path) {
                out.push(SkillFileEntry { rel_path, bytes });
            }
        }
    }
}

pub(crate) fn load_embedded_skill_packages() -> Vec<SkillPackage> {
    let mut grouped: BTreeMap<String, Vec<SkillFileEntry>> = BTreeMap::new();

    for path in EmbeddedDiscoverySkills::iter() {
        if let Some((dir_name, rel_path)) = path.as_ref().split_once('/') {
            if let Some(asset) = EmbeddedDiscoverySkills::get(path.as_ref()) {
                grouped
                    .entry(dir_name.to_string())
                    .or_default()
                    .push(SkillFileEntry {
                        rel_path: rel_path.to_string(),
                        bytes: asset.data.into_owned(),
                    });
            }
        }
    }

    for path in EmbeddedSkillData::iter() {
        if let Some((dir_name, rel_path)) = path.as_ref().split_once('/') {
            if let Some(asset) = EmbeddedSkillData::get(path.as_ref()) {
                grouped
                    .entry(dir_name.to_string())
                    .or_default()
                    .push(SkillFileEntry {
                        rel_path: rel_path.to_string(),
                        bytes: asset.data.into_owned(),
                    });
            }
        }
    }

    let mut by_name: BTreeMap<String, SkillPackage> = BTreeMap::new();
    for (_dir_name, mut files) in grouped {
        files.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
        let Some(skill_md) = files.iter().find(|f| f.rel_path == "SKILL.md") else {
            continue;
        };
        let Ok(md_text) = std::str::from_utf8(&skill_md.bytes) else {
            continue;
        };
        let Some(frontmatter) = parse_frontmatter_json(md_text) else {
            continue;
        };
        let Some(name) = frontmatter
            .get("name")
            .and_then(Value::as_str)
            .map(ToString::to_string)
        else {
            continue;
        };
        let description = frontmatter
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let hidden = frontmatter
            .get("hidden")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        by_name.insert(
            name.clone(),
            SkillPackage {
                name,
                description,
                hidden,
                frontmatter,
                files,
            },
        );
    }
    by_name.into_values().collect()
}

/// Parse YAML frontmatter from a SKILL.md file. Returns (name, description, hidden).
fn parse_frontmatter(content: &str) -> Option<(String, String, bool)> {
    let content = content.trim_start();
    if !content.starts_with("---") {
        return None;
    }
    let after_opening = &content[3..];
    let end = after_opening.find("\n---")?;
    let frontmatter = &after_opening[..end];

    let mut name = None;
    let mut description = None;
    let mut hidden = false;

    let lines: Vec<&str> = frontmatter.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if let Some(val) = line.strip_prefix("name:") {
            name = Some(val.trim().to_string());
        } else if let Some(val) = line.strip_prefix("description:") {
            let mut desc = val.trim().to_string();
            // Consume YAML continuation lines (indented with spaces or tab)
            while i + 1 < lines.len()
                && (lines[i + 1].starts_with("  ") || lines[i + 1].starts_with('\t'))
            {
                i += 1;
                desc.push(' ');
                desc.push_str(lines[i].trim());
            }
            description = Some(desc);
        } else if let Some(val) = line.strip_prefix("hidden:") {
            hidden = matches!(val.trim(), "true" | "yes");
        }
        i += 1;
    }

    Some((name?, description.unwrap_or_default(), hidden))
}

/// Discover all skills across the given directories.
fn skill_not_found(name: &str) -> String {
    format!(
        "Skill not found: {}. Run `agent-browser skills list` to see available skills.",
        name
    )
}

fn discover_skills(dirs: &[PathBuf]) -> Vec<SkillInfo> {
    let mut skills = Vec::new();

    for skills_dir in dirs {
        let entries = match fs::read_dir(skills_dir) {
            Ok(e) => e,
            Err(_) => continue,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let skill_md = path.join("SKILL.md");
            if !skill_md.exists() {
                continue;
            }
            let content = match fs::read_to_string(&skill_md) {
                Ok(c) => c,
                Err(_) => continue,
            };
            if let Some((name, description, hidden)) = parse_frontmatter(&content) {
                skills.push(SkillInfo {
                    name,
                    description,
                    dir: path,
                    hidden,
                });
            }
        }
    }

    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

fn truncate_description(desc: &str, max_len: usize) -> String {
    if desc.len() <= max_len {
        return desc.to_string();
    }
    let boundary = desc
        .char_indices()
        .take_while(|(i, _)| *i <= max_len)
        .last()
        .map(|(i, _)| i)
        .unwrap_or(max_len);
    let end = desc[..boundary].rfind(' ').unwrap_or(boundary);
    format!("{}...", &desc[..end])
}

/// Read the full SKILL.md content (including frontmatter).
fn read_skill_full(skill_md: &Path) -> Option<String> {
    fs::read_to_string(skill_md).ok()
}

/// Collect all supplementary files (references/, templates/) for a skill.
fn collect_supplementary_files(skill_dir: &Path) -> Vec<(String, String)> {
    let mut files = Vec::new();
    for subdir_name in &["references", "templates"] {
        let subdir = skill_dir.join(subdir_name);
        if !subdir.is_dir() {
            continue;
        }
        let mut entries: Vec<_> = match fs::read_dir(&subdir) {
            Ok(e) => e.flatten().collect(),
            Err(_) => continue,
        };
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let path = entry.path();
            if path.is_file() {
                if let Ok(content) = fs::read_to_string(&path) {
                    let rel = format!(
                        "{}/{}",
                        subdir_name,
                        path.file_name().unwrap_or_default().to_string_lossy()
                    );
                    files.push((rel, content));
                }
            }
        }
    }
    files
}

/// Resolve a `--ref` value against a skill's supplementary files. Accepts the
/// relative path (`references/commands.md`), the file name (`commands.md`), or
/// the file stem (`commands`). Matching only against collected files keeps the
/// lookup inside the skill directory.
fn resolve_ref<'a>(
    files: &'a [(String, String)],
    wanted: &str,
) -> Result<&'a (String, String), String> {
    let wanted = wanted.trim_start_matches("./");
    let matches: Vec<&(String, String)> = files
        .iter()
        .filter(|(path, _)| {
            let file_name = path.rsplit('/').next().unwrap_or(path);
            let stem = file_name.split('.').next().unwrap_or(file_name);
            path == wanted || file_name == wanted || stem == wanted
        })
        .collect();
    match matches.as_slice() {
        [one] => Ok(one),
        [] => Err(format!("Reference not found: {}", wanted)),
        many => Err(format!(
            "Ambiguous reference {}: matches {}",
            wanted,
            many.iter()
                .map(|(path, _)| path.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

fn print_error(json_mode: bool, message: &str) {
    if json_mode {
        println!(
            "{}",
            serde_json::to_string(&json!({ "success": false, "error": message }))
                .unwrap_or_default()
        );
    } else {
        eprintln!("{} {}", color::error_indicator(), message);
    }
}

fn run_get_refs(skill: &SkillInfo, refs: &[String], json_mode: bool) {
    let files = collect_supplementary_files(&skill.dir);
    let mut selected = Vec::new();
    for wanted in refs {
        match resolve_ref(&files, wanted) {
            Ok(file) => {
                if !selected.contains(&file) {
                    selected.push(file);
                }
            }
            Err(message) => {
                let available = if files.is_empty() {
                    format!("{} has no references", skill.name)
                } else {
                    format!(
                        "Available: {}",
                        files
                            .iter()
                            .map(|(path, _)| path.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                };
                print_error(json_mode, &format!("{}. {}", message, available));
                exit(1);
            }
        }
    }

    if json_mode {
        let files: Vec<serde_json::Value> = selected
            .iter()
            .map(|(path, content)| json!({ "path": path, "content": content }))
            .collect();
        println!(
            "{}",
            serde_json::to_string(&json!({
                "success": true,
                "data": [{ "name": skill.name, "files": files }],
            }))
            .unwrap_or_default()
        );
    } else {
        for (i, (path, content)) in selected.iter().enumerate() {
            if selected.len() > 1 {
                if i > 0 {
                    println!();
                }
                println!("--- {} ---\n", path);
            }
            print!("{}", content);
            if !content.ends_with('\n') {
                println!();
            }
        }
    }
}

fn run_list(skills_dirs: &[PathBuf], json_mode: bool) {
    let skills: Vec<SkillInfo> = discover_skills(skills_dirs)
        .into_iter()
        .filter(|s| !s.hidden)
        .collect();
    if skills.is_empty() {
        if json_mode {
            println!(
                "{}",
                serde_json::to_string(&json!({ "success": true, "data": [] })).unwrap_or_default()
            );
        } else {
            println!("No skills found");
        }
        return;
    }

    if json_mode {
        let items: Vec<serde_json::Value> = skills
            .iter()
            .map(|s| {
                json!({
                    "name": s.name,
                    "description": s.description,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string(&json!({ "success": true, "data": items })).unwrap_or_default()
        );
    } else {
        let max_name = skills.iter().map(|s| s.name.len()).max().unwrap_or(0);
        for s in &skills {
            println!(
                "  {:<width$}  {}",
                s.name,
                truncate_description(&s.description, 70),
                width = max_name
            );
        }
    }
}

#[derive(Debug, Default, PartialEq)]
struct GetArgs {
    names: Vec<String>,
    refs: Vec<String>,
    ignored: Vec<String>,
    full: bool,
    all: bool,
}

/// Parses `skills get` operands. Everything after `--` is a skill name.
fn parse_get_args(operands: &[String]) -> Result<GetArgs, &'static str> {
    let mut parsed = GetArgs::default();
    let mut rest = operands.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--" => {
                parsed.names.extend(rest.by_ref().cloned());
                break;
            }
            "--full" => parsed.full = true,
            "--all" => parsed.all = true,
            "--ref" => {
                match rest.next() {
                    Some(value) => parsed.refs.push(value.clone()),
                    None => return Err(
                        "--ref needs a value. Usage: agent-browser skills get <name> --ref <ref>",
                    ),
                }
            }
            other => match other.strip_prefix("--ref=") {
                Some(value) => parsed.refs.push(value.to_string()),
                None if other.starts_with('-') => parsed.ignored.push(arg.clone()),
                None => parsed.names.push(arg.clone()),
            },
        }
    }
    Ok(parsed)
}

fn run_get(
    skills_dirs: &[PathBuf],
    names: &[String],
    get_all: bool,
    full: bool,
    refs: &[String],
    json_mode: bool,
) {
    let all_skills = discover_skills(skills_dirs);

    let targets: Vec<&SkillInfo> = if get_all {
        all_skills.iter().filter(|s| !s.hidden).collect()
    } else {
        let mut targets = Vec::new();
        for name in names {
            match all_skills.iter().find(|s| s.name == *name) {
                Some(s) => targets.push(s),
                None => {
                    if json_mode {
                        println!(
                            "{}",
                            serde_json::to_string(&json!({
                                "success": false,
                                "error": skill_not_found(name),
                            }))
                            .unwrap_or_default()
                        );
                    } else {
                        eprintln!("{} {}", color::error_indicator(), skill_not_found(name));
                    }
                    exit(1);
                }
            }
        }
        targets
    };

    if targets.is_empty() {
        if json_mode {
            println!(
                "{}",
                serde_json::to_string(&json!({
                    "success": false,
                    "error": "No skill name provided. Usage: agent-browser skills get <name>",
                }))
                .unwrap_or_default()
            );
        } else {
            eprintln!(
                "{} No skill name provided. Usage: agent-browser skills get <name>",
                color::error_indicator()
            );
        }
        exit(1);
    }

    if !refs.is_empty() {
        if get_all || full || targets.len() != 1 {
            print_error(
                json_mode,
                "--ref needs exactly one skill name and cannot be combined with --all or --full. Usage: agent-browser skills get <name> --ref <ref>",
            );
            exit(1);
        }
        run_get_refs(targets[0], refs, json_mode);
        return;
    }

    if json_mode {
        let items: Vec<serde_json::Value> = targets
            .iter()
            .map(|s| {
                let skill_md = s.dir.join("SKILL.md");
                let content = read_skill_full(&skill_md).unwrap_or_default();
                let mut obj = json!({
                    "name": s.name,
                    "content": content,
                });
                if full {
                    let supplementary = collect_supplementary_files(&s.dir);
                    if !supplementary.is_empty() {
                        let files: Vec<serde_json::Value> = supplementary
                            .iter()
                            .map(|(path, content)| json!({ "path": path, "content": content }))
                            .collect();
                        obj["files"] = json!(files);
                    }
                }
                obj
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string(&json!({ "success": true, "data": items })).unwrap_or_default()
        );
    } else {
        for (i, s) in targets.iter().enumerate() {
            if i > 0 {
                println!("\n---\n");
            }
            let skill_md = s.dir.join("SKILL.md");
            if let Some(content) = read_skill_full(&skill_md) {
                print!("{}", content);
                if !content.ends_with('\n') {
                    println!();
                }
            }
            if full {
                let supplementary = collect_supplementary_files(&s.dir);
                for (path, content) in &supplementary {
                    println!("\n--- {} ---\n", path);
                    print!("{}", content);
                    if !content.ends_with('\n') {
                        println!();
                    }
                }
            }
        }
    }
}

fn run_path(skills_dirs: &[PathBuf], name: Option<&str>, json_mode: bool) {
    match name {
        Some(name) => {
            let all_skills = discover_skills(skills_dirs);
            match all_skills.iter().find(|s| s.name == name) {
                Some(s) => {
                    let path = s.dir.to_string_lossy().to_string();
                    if json_mode {
                        println!(
                            "{}",
                            serde_json::to_string(&json!({
                                "success": true,
                                "data": { "name": s.name, "path": path },
                            }))
                            .unwrap_or_default()
                        );
                    } else {
                        println!("{}", path);
                    }
                }
                None => {
                    if json_mode {
                        println!(
                            "{}",
                            serde_json::to_string(&json!({
                                "success": false,
                                "error": skill_not_found(name),
                            }))
                            .unwrap_or_default()
                        );
                    } else {
                        eprintln!("{} {}", color::error_indicator(), skill_not_found(name));
                    }
                    exit(1);
                }
            }
        }
        None => {
            let paths: Vec<String> = skills_dirs
                .iter()
                .map(|d| d.to_string_lossy().to_string())
                .collect();
            if json_mode {
                println!(
                    "{}",
                    serde_json::to_string(&json!({
                        "success": true,
                        "data": { "paths": paths },
                    }))
                    .unwrap_or_default()
                );
            } else {
                for p in &paths {
                    println!("{}", p);
                }
            }
        }
    }
}

pub fn run_skills(args: &[String], json_mode: bool) {
    let skills_dirs = find_skills_dirs();
    if skills_dirs.is_empty() {
        if json_mode {
            println!(
                "{}",
                serde_json::to_string(&json!({
                    "success": false,
                    "error": "Skills directory not found. Install skills under ~/.agent-browser/ or set AGENT_BROWSER_SKILLS_DIR.",
                }))
                .unwrap_or_default()
            );
        } else {
            eprintln!(
                "{} Skills directory not found. Install skills under ~/.agent-browser/ or set AGENT_BROWSER_SKILLS_DIR.",
                color::error_indicator()
            );
        }
        exit(1);
    }

    let subcommand = args.get(1).map(|s| s.as_str());

    match subcommand {
        None | Some("list") => run_list(&skills_dirs, json_mode),
        Some("get") => {
            let parsed = match parse_get_args(&args[2..]) {
                Ok(parsed) => parsed,
                Err(message) => {
                    print_error(json_mode, message);
                    exit(1);
                }
            };
            for flag in &parsed.ignored {
                eprintln!(
                    "{} Unknown flag ignored: {}",
                    color::warning_indicator(),
                    flag
                );
            }
            run_get(
                &skills_dirs,
                &parsed.names,
                parsed.all,
                parsed.full,
                &parsed.refs,
                json_mode,
            );
        }
        Some("path") => {
            let mut operands = args[2..].iter().map(|s| s.as_str());
            let mut name = operands.next();
            if name == Some("--") {
                name = operands.next().or(name);
            }
            run_path(&skills_dirs, name, json_mode);
        }
        Some(unknown) => {
            if json_mode {
                println!(
                    "{}",
                    serde_json::to_string(&json!({
                        "success": false,
                        "error": format!("Unknown skills subcommand: {}", unknown),
                    }))
                    .unwrap_or_default()
                );
            } else {
                eprintln!(
                    "{} Unknown skills subcommand: {}",
                    color::error_indicator(),
                    unknown
                );
            }
            exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn create_test_skill(dir: &Path, name: &str, description: &str) {
        let skill_dir = dir.join(name);
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            format!(
                "---\nname: {}\ndescription: {}\n---\n\n# {}\n\nContent here.\n",
                name, description, name
            ),
        )
        .unwrap();
    }

    #[test]
    fn get_operands_after_the_marker_are_names() {
        let args = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let parsed = parse_get_args(&args(&["--full", "--", "--all", "-x"])).unwrap();
        assert_eq!(parsed.names, ["--all", "-x"]);
        assert!(parsed.full && !parsed.all && parsed.ignored.is_empty());
        let parsed = parse_get_args(&args(&["--foo", "core"])).unwrap();
        assert_eq!(parsed.ignored, ["--foo"]);
        assert_eq!(parsed.names, ["core"]);
    }

    #[test]
    fn test_parse_frontmatter_basic() {
        let content = "---\nname: test-skill\ndescription: A test skill.\n---\n\n# Test\n";
        let (name, desc, hidden) = parse_frontmatter(content).unwrap();
        assert_eq!(name, "test-skill");
        assert_eq!(desc, "A test skill.");
        assert!(!hidden);
    }

    #[test]
    fn test_parse_frontmatter_multiline_description() {
        let content =
            "---\nname: test\ndescription: First line\n  continued here\n  and here\n---\n";
        let (name, desc, hidden) = parse_frontmatter(content).unwrap();
        assert_eq!(name, "test");
        assert_eq!(desc, "First line continued here and here");
        assert!(!hidden);
    }

    #[test]
    fn test_parse_frontmatter_hidden_true() {
        let content = "---\nname: stub\ndescription: A bootstrap stub.\nhidden: true\n---\n";
        let (name, desc, hidden) = parse_frontmatter(content).unwrap();
        assert_eq!(name, "stub");
        assert_eq!(desc, "A bootstrap stub.");
        assert!(hidden);
    }

    #[test]
    fn test_parse_frontmatter_hidden_false() {
        let content = "---\nname: visible\ndescription: Visible.\nhidden: false\n---\n";
        let (_, _, hidden) = parse_frontmatter(content).unwrap();
        assert!(!hidden);
    }

    #[test]
    fn test_parse_frontmatter_no_frontmatter() {
        let content = "# Just a heading\n\nNo frontmatter here.\n";
        assert!(parse_frontmatter(content).is_none());
    }

    #[test]
    fn test_parse_frontmatter_missing_name() {
        let content = "---\ndescription: No name field\n---\n";
        assert!(parse_frontmatter(content).is_none());
    }

    #[test]
    fn test_discover_skills_single_dir() {
        let tmp = tempfile::tempdir().unwrap();
        create_test_skill(tmp.path(), "alpha", "Alpha skill");
        create_test_skill(tmp.path(), "beta", "Beta skill");

        // Non-skill directory (no SKILL.md)
        fs::create_dir_all(tmp.path().join("not-a-skill")).unwrap();
        fs::write(tmp.path().join("not-a-skill").join("README.md"), "hi").unwrap();

        let dirs = vec![tmp.path().to_path_buf()];
        let skills = discover_skills(&dirs);
        assert_eq!(skills.len(), 2);
        assert_eq!(skills[0].name, "alpha");
        assert_eq!(skills[1].name, "beta");
    }

    #[test]
    fn test_discover_skills_multiple_dirs() {
        let tmp1 = tempfile::tempdir().unwrap();
        let tmp2 = tempfile::tempdir().unwrap();
        create_test_skill(tmp1.path(), "alpha", "Alpha skill");
        create_test_skill(tmp2.path(), "beta", "Beta skill");
        create_test_skill(tmp2.path(), "gamma", "Gamma skill");

        let dirs = vec![tmp1.path().to_path_buf(), tmp2.path().to_path_buf()];
        let skills = discover_skills(&dirs);
        assert_eq!(skills.len(), 3);
        assert_eq!(skills[0].name, "alpha");
        assert_eq!(skills[1].name, "beta");
        assert_eq!(skills[2].name, "gamma");
    }

    #[test]
    fn test_truncate_description() {
        assert_eq!(truncate_description("short", 10), "short");
        assert_eq!(
            truncate_description("this is a longer description that should be truncated", 20),
            "this is a longer..."
        );
    }

    #[test]
    fn test_truncate_description_multibyte() {
        let desc = "Browse \u{00e9}l\u{00e9}ments and \u{65e5}\u{672c}\u{8a9e} pages quickly";
        let result = truncate_description(desc, 20);
        assert!(result.ends_with("..."));
        assert!(result.len() <= 30);
    }

    #[test]
    fn test_collect_supplementary_files() {
        let tmp = tempfile::tempdir().unwrap();
        let refs_dir = tmp.path().join("references");
        fs::create_dir_all(&refs_dir).unwrap();
        fs::write(refs_dir.join("auth.md"), "# Auth\n").unwrap();
        fs::write(refs_dir.join("commands.md"), "# Commands\n").unwrap();

        let templates_dir = tmp.path().join("templates");
        fs::create_dir_all(&templates_dir).unwrap();
        fs::write(templates_dir.join("example.sh"), "#!/bin/bash\n").unwrap();

        let files = collect_supplementary_files(tmp.path());
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].0, "references/auth.md");
        assert_eq!(files[1].0, "references/commands.md");
        assert_eq!(files[2].0, "templates/example.sh");
    }

    #[test]
    fn test_resolve_ref_accepts_path_file_name_and_stem() {
        let files = vec![
            (
                "references/commands.md".to_string(),
                "# Commands\n".to_string(),
            ),
            (
                "templates/form-automation.sh".to_string(),
                "#!/bin/bash\n".to_string(),
            ),
        ];
        for wanted in [
            "references/commands.md",
            "./references/commands.md",
            "commands.md",
            "commands",
        ] {
            assert_eq!(
                resolve_ref(&files, wanted).unwrap().0,
                "references/commands.md"
            );
        }
        assert_eq!(
            resolve_ref(&files, "form-automation").unwrap().0,
            "templates/form-automation.sh"
        );
    }

    #[test]
    fn test_resolve_ref_rejects_missing_ambiguous_and_outside_paths() {
        let files = vec![
            ("references/auth.md".to_string(), String::new()),
            ("templates/auth.sh".to_string(), String::new()),
        ];
        assert!(resolve_ref(&files, "missing")
            .unwrap_err()
            .contains("not found"));
        assert!(resolve_ref(&files, "auth")
            .unwrap_err()
            .contains("Ambiguous"));
        assert!(resolve_ref(&files, "../SKILL.md").is_err());
        assert!(resolve_ref(&files, "/etc/passwd").is_err());
    }

    #[test]
    fn test_bundled_core_skill_routes_every_reference_and_skill() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../skill-data");
        let core = fs::read_to_string(root.join("core/SKILL.md")).unwrap();
        for (path, _) in collect_supplementary_files(&root.join("core")) {
            if path.starts_with("references/") {
                assert!(
                    core.contains(&format!("`{}`", path)),
                    "core SKILL.md does not route to {}",
                    path
                );
            }
        }
        for skill in discover_skills(&[root]) {
            if skill.name != "core" && !skill.hidden {
                assert!(
                    core.contains(&format!("| `{}` |", skill.name)),
                    "core SKILL.md does not route to the {} skill",
                    skill.name
                );
            }
        }
    }
}
