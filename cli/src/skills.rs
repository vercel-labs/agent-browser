use serde_json::json;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::exit;

use crate::color;

struct SkillInfo {
    name: String,
    description: String,
    dir: PathBuf,
}

/// Locate the `skills/` directory bundled with the installation.
///
/// Resolution order:
/// 1. AGENT_BROWSER_SKILLS_DIR env var
/// 2. ../skills/ relative to the executable (npm installs: binary is in bin/)
/// 3. Walk up from the executable to find a project root with skills/
///    (dev builds where binary is in target/debug/ or target/release/)
fn find_skills_dir() -> Option<PathBuf> {
    if let Ok(dir) = env::var("AGENT_BROWSER_SKILLS_DIR") {
        let p = PathBuf::from(dir);
        if p.is_dir() {
            return Some(p);
        }
    }

    if let Ok(exe) = env::current_exe() {
        let exe = exe.canonicalize().unwrap_or(exe);
        if let Some(parent) = exe.parent() {
            // npm install layout: bin/agent-browser-* -> ../skills/
            let candidate = parent.join("..").join("skills");
            if candidate.is_dir() {
                return Some(candidate);
            }

            // dev build layout: walk up from target/debug/ or target/release/
            let mut dir = parent;
            loop {
                let candidate = dir.join("skills");
                if candidate.is_dir() {
                    return Some(candidate);
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

/// Parse YAML frontmatter from a SKILL.md file. Returns (name, description).
fn parse_frontmatter(content: &str) -> Option<(String, String)> {
    let content = content.trim_start();
    if !content.starts_with("---") {
        return None;
    }
    let after_opening = &content[3..];
    let end = after_opening.find("\n---")?;
    let frontmatter = &after_opening[..end];

    let mut name = None;
    let mut description = None;

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
        }
        i += 1;
    }

    Some((name?, description.unwrap_or_default()))
}

/// Discover all skills in the skills directory.
fn discover_skills(skills_dir: &Path) -> Vec<SkillInfo> {
    let mut skills = Vec::new();
    let entries = match fs::read_dir(skills_dir) {
        Ok(e) => e,
        Err(_) => return skills,
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
        if let Some((name, description)) = parse_frontmatter(&content) {
            skills.push(SkillInfo {
                name,
                description,
                dir: path,
            });
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

fn run_list(skills_dir: &Path, json_mode: bool) {
    let skills = discover_skills(skills_dir);
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

fn run_get(skills_dir: &Path, names: &[String], get_all: bool, full: bool, json_mode: bool) {
    let all_skills = discover_skills(skills_dir);

    let targets: Vec<&SkillInfo> = if get_all {
        all_skills.iter().collect()
    } else {
        let mut targets = Vec::new();
        for name in names {
            if name.starts_with('-') {
                eprintln!(
                    "{} Unknown flag ignored: {}",
                    color::warning_indicator(),
                    name
                );
                continue;
            }
            match all_skills.iter().find(|s| s.name == *name) {
                Some(s) => targets.push(s),
                None => {
                    if json_mode {
                        println!(
                            "{}",
                            serde_json::to_string(&json!({
                                "success": false,
                                "error": format!("Skill not found: {}", name),
                            }))
                            .unwrap_or_default()
                        );
                    } else {
                        eprintln!("{} Skill not found: {}", color::error_indicator(), name);
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

fn run_path(skills_dir: &Path, name: Option<&str>, json_mode: bool) {
    match name {
        Some(name) => {
            let all_skills = discover_skills(skills_dir);
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
                                "error": format!("Skill not found: {}", name),
                            }))
                            .unwrap_or_default()
                        );
                    } else {
                        eprintln!("{} Skill not found: {}", color::error_indicator(), name);
                    }
                    exit(1);
                }
            }
        }
        None => {
            let path = skills_dir.to_string_lossy().to_string();
            if json_mode {
                println!(
                    "{}",
                    serde_json::to_string(&json!({
                        "success": true,
                        "data": { "path": path },
                    }))
                    .unwrap_or_default()
                );
            } else {
                println!("{}", path);
            }
        }
    }
}

pub fn run_skills(args: &[String], json_mode: bool) {
    let skills_dir = match find_skills_dir() {
        Some(d) => d.canonicalize().unwrap_or(d),
        None => {
            if json_mode {
                println!(
                    "{}",
                    serde_json::to_string(&json!({
                        "success": false,
                        "error": "Skills directory not found. Set AGENT_BROWSER_SKILLS_DIR or reinstall via npm.",
                    }))
                    .unwrap_or_default()
                );
            } else {
                eprintln!(
                    "{} Skills directory not found. Set AGENT_BROWSER_SKILLS_DIR or reinstall via npm.",
                    color::error_indicator()
                );
            }
            exit(1);
        }
    };

    let subcommand = args.get(1).map(|s| s.as_str());

    match subcommand {
        None | Some("list") => run_list(&skills_dir, json_mode),
        Some("get") => {
            let names: Vec<String> = args[2..]
                .iter()
                .filter(|a| *a != "--full" && *a != "--all")
                .cloned()
                .collect();
            let full = args[2..].iter().any(|a| a == "--full");
            let get_all = args[2..].iter().any(|a| a == "--all");
            run_get(&skills_dir, &names, get_all, full, json_mode);
        }
        Some("path") => {
            let name = args.get(2).map(|s| s.as_str());
            run_path(&skills_dir, name, json_mode);
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
    fn test_parse_frontmatter_basic() {
        let content = "---\nname: test-skill\ndescription: A test skill.\n---\n\n# Test\n";
        let (name, desc) = parse_frontmatter(content).unwrap();
        assert_eq!(name, "test-skill");
        assert_eq!(desc, "A test skill.");
    }

    #[test]
    fn test_parse_frontmatter_multiline_description() {
        let content =
            "---\nname: test\ndescription: First line\n  continued here\n  and here\n---\n";
        let (name, desc) = parse_frontmatter(content).unwrap();
        assert_eq!(name, "test");
        assert_eq!(desc, "First line continued here and here");
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
    fn test_discover_skills() {
        let tmp = tempfile::tempdir().unwrap();
        create_test_skill(tmp.path(), "alpha", "Alpha skill");
        create_test_skill(tmp.path(), "beta", "Beta skill");

        // Non-skill directory (no SKILL.md)
        fs::create_dir_all(tmp.path().join("not-a-skill")).unwrap();
        fs::write(tmp.path().join("not-a-skill").join("README.md"), "hi").unwrap();

        let skills = discover_skills(tmp.path());
        assert_eq!(skills.len(), 2);
        assert_eq!(skills[0].name, "alpha");
        assert_eq!(skills[1].name, "beta");
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
}
