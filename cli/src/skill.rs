use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::exit;

/// Skill installation scope
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SkillScope {
    User,
    Project,
}

impl SkillScope {
    fn as_str(&self) -> &'static str {
        match self {
            SkillScope::User => "user",
            SkillScope::Project => "project",
        }
    }
}

/// Get the source directory containing the skill files
fn get_skill_source_dir() -> Option<PathBuf> {
    // Try to find skills directory relative to the executable
    if let Ok(exe_path) = env::current_exe() {
        // Check ../skills (installed via npm)
        if let Some(parent) = exe_path.parent() {
            let skills_dir = parent.join("../skills/agent-browser");
            if skills_dir.exists() {
                return Some(skills_dir.canonicalize().unwrap_or(skills_dir));
            }
            // Also check ../../skills for development
            let dev_skills_dir = parent.join("../../skills/agent-browser");
            if dev_skills_dir.exists() {
                return Some(dev_skills_dir.canonicalize().unwrap_or(dev_skills_dir));
            }
        }
    }

    // Check current working directory (for development)
    let cwd_skills = PathBuf::from("skills/agent-browser");
    if cwd_skills.exists() {
        return Some(cwd_skills.canonicalize().unwrap_or(cwd_skills));
    }

    // Check AGENT_BROWSER_SKILLS_DIR environment variable
    if let Ok(skills_dir) = env::var("AGENT_BROWSER_SKILLS_DIR") {
        let path = PathBuf::from(skills_dir);
        if path.exists() {
            return Some(path);
        }
    }

    None
}

/// Get the target directory for skill installation
fn get_skill_target_dir(scope: SkillScope) -> PathBuf {
    match scope {
        SkillScope::User => {
            let home = env::var("HOME")
                .or_else(|_| env::var("USERPROFILE"))
                .unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".claude/skills/agent-browser")
        }
        SkillScope::Project => PathBuf::from(".claude/skills/agent-browser"),
    }
}

/// Check if skill is installed at a given scope
fn is_installed(scope: SkillScope) -> bool {
    let target = get_skill_target_dir(scope);
    target.join("SKILL.md").exists()
}

/// Install skill files to target directory
fn install_skill_to(scope: SkillScope, force: bool) -> Result<(), String> {
    let source_dir = get_skill_source_dir().ok_or_else(|| {
        "Could not find skill source files. Make sure agent-browser is properly installed.".to_string()
    })?;

    let target_dir = get_skill_target_dir(scope);

    // Check if already installed
    if target_dir.exists() && !force {
        return Err(format!(
            "Skill already installed at {} scope. Use --force to overwrite.",
            scope.as_str()
        ));
    }

    // Create target directory
    fs::create_dir_all(&target_dir).map_err(|e| format!("Failed to create directory: {}", e))?;

    // Copy .md files
    let entries = fs::read_dir(&source_dir).map_err(|e| format!("Failed to read source dir: {}", e))?;

    let mut copied = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().map(|e| e == "md").unwrap_or(false) {
            let file_name = path.file_name().unwrap();
            let dest = target_dir.join(file_name);
            fs::copy(&path, &dest).map_err(|e| format!("Failed to copy {:?}: {}", file_name, e))?;
            copied += 1;
        }
    }

    if copied == 0 {
        return Err("No skill files found to copy".to_string());
    }

    Ok(())
}

/// Prompt user for confirmation
fn confirm_action(prompt: &str) -> bool {
    print!("{} [y/N] ", prompt);
    io::stdout().flush().ok();

    let mut input = String::new();
    if io::stdin().read_line(&mut input).is_err() {
        return false;
    }

    matches!(input.trim().to_lowercase().as_str(), "y" | "yes")
}

/// Uninstall skill from target directory
fn uninstall_skill_from(scope: SkillScope, force: bool, json_mode: bool) -> Result<(), String> {
    let target_dir = get_skill_target_dir(scope);

    if !target_dir.exists() {
        return Err(format!("Skill not installed at {} scope.", scope.as_str()));
    }

    if !force {
        // In JSON mode, require --force (no interactive prompt)
        if json_mode {
            return Err(format!(
                "Use --force to confirm removal of skill from {} scope.",
                scope.as_str()
            ));
        }

        // Interactive confirmation
        let prompt = format!(
            "Remove skill from {} scope ({})?",
            scope.as_str(),
            target_dir.display()
        );
        if !confirm_action(&prompt) {
            return Err("Aborted.".to_string());
        }
    }

    fs::remove_dir_all(&target_dir).map_err(|e| format!("Failed to remove directory: {}", e))?;

    Ok(())
}

/// Run the skill command
pub fn run_skill(args: &[String], json_mode: bool) {
    let subcommand = args.get(1).map(|s| s.as_str());
    let force = args.iter().any(|a| a == "--force" || a == "-f");
    let project_scope = args.iter().any(|a| a == "--project" || a == "-p");

    // Default to user scope unless --project is specified
    let scope = if project_scope {
        SkillScope::Project
    } else {
        SkillScope::User
    };

    match subcommand {
        Some("install") => {
            match install_skill_to(scope, force) {
                Ok(()) => {
                    let target = get_skill_target_dir(scope);
                    if json_mode {
                        println!(
                            r#"{{"success":true,"message":"Skill installed to {} scope","path":"{}"}}"#,
                            scope.as_str(),
                            target.display()
                        );
                    } else {
                        println!("\x1b[32m✓\x1b[0m Skill installed to {} scope", scope.as_str());
                        println!("  Path: {}", target.display());
                    }
                }
                Err(e) => {
                    if json_mode {
                        println!(r#"{{"success":false,"error":"{}"}}"#, e);
                    } else {
                        eprintln!("\x1b[31m✗\x1b[0m {}", e);
                    }
                    exit(1);
                }
            }
        }

        Some("uninstall") => {
            match uninstall_skill_from(scope, force, json_mode) {
                Ok(()) => {
                    if json_mode {
                        println!(
                            r#"{{"success":true,"message":"Skill uninstalled from {} scope"}}"#,
                            scope.as_str()
                        );
                    } else {
                        println!("\x1b[32m✓\x1b[0m Skill uninstalled from {} scope", scope.as_str());
                    }
                }
                Err(e) => {
                    if json_mode {
                        println!(r#"{{"success":false,"error":"{}"}}"#, e);
                    } else {
                        eprintln!("\x1b[31m✗\x1b[0m {}", e);
                    }
                    exit(1);
                }
            }
        }

        Some("status") => {
            let user_installed = is_installed(SkillScope::User);
            let project_installed = is_installed(SkillScope::Project);
            let user_path = get_skill_target_dir(SkillScope::User);
            let project_path = get_skill_target_dir(SkillScope::Project);

            if json_mode {
                println!(
                    r#"{{"success":true,"data":{{"user":{{"installed":{},"path":"{}"}},"project":{{"installed":{},"path":"{}"}}}}}}"#,
                    user_installed,
                    user_path.display(),
                    project_installed,
                    project_path.display()
                );
            } else {
                println!("Skill installation status:");
                println!();
                let user_mark = if user_installed { "\x1b[32m✓\x1b[0m" } else { "\x1b[90m○\x1b[0m" };
                let project_mark = if project_installed { "\x1b[32m✓\x1b[0m" } else { "\x1b[90m○\x1b[0m" };
                println!("{} User scope:    {}", user_mark, user_path.display());
                println!("{} Project scope: {}", project_mark, project_path.display());
            }
        }

        Some("show") => {
            let source_dir = get_skill_source_dir();
            match source_dir {
                Some(dir) => {
                    let skill_file = dir.join("SKILL.md");
                    match fs::read_to_string(&skill_file) {
                        Ok(content) => {
                            if json_mode {
                                println!(
                                    r#"{{"success":true,"data":{{"path":"{}","content":{}}}}}"#,
                                    skill_file.display(),
                                    serde_json::to_string(&content).unwrap_or_default()
                                );
                            } else {
                                println!("{}", content);
                            }
                        }
                        Err(e) => {
                            if json_mode {
                                println!(r#"{{"success":false,"error":"Failed to read skill file: {}"}}"#, e);
                            } else {
                                eprintln!("\x1b[31m✗\x1b[0m Failed to read skill file: {}", e);
                            }
                            exit(1);
                        }
                    }
                }
                None => {
                    if json_mode {
                        println!(r#"{{"success":false,"error":"Could not find skill source files"}}"#);
                    } else {
                        eprintln!("\x1b[31m✗\x1b[0m Could not find skill source files");
                    }
                    exit(1);
                }
            }
        }

        None | Some("help") | Some("--help") | Some("-h") => {
            if json_mode {
                println!(r#"{{"success":true,"data":{{"commands":["install","uninstall","status","show"]}}}}"#);
            } else {
                print_skill_help();
            }
        }

        Some(unknown) => {
            if json_mode {
                println!(
                    r#"{{"success":false,"error":"Unknown skill subcommand: {}","valid":["install","uninstall","status","show"]}}"#,
                    unknown
                );
            } else {
                eprintln!("\x1b[31m✗\x1b[0m Unknown skill subcommand: {}", unknown);
                eprintln!("Valid subcommands: install, uninstall, status, show");
            }
            exit(1);
        }
    }
}

fn print_skill_help() {
    println!("\x1b[1magent-browser skill\x1b[0m - Manage Claude Code skill integration");
    println!();
    println!("\x1b[1mUSAGE:\x1b[0m");
    println!("  agent-browser skill <command> [options]");
    println!();
    println!("\x1b[1mCOMMANDS:\x1b[0m");
    println!("  install     Install skill to Claude Code");
    println!("  uninstall   Remove installed skill");
    println!("  status      Show installation status");
    println!("  show        Display skill file content");
    println!();
    println!("\x1b[1mOPTIONS:\x1b[0m");
    println!("  --user, -u      Install to user scope (~/.claude/skills/)");
    println!("  --project, -p   Install to project scope (.claude/skills/)");
    println!("  --force, -f     Overwrite existing installation or confirm removal");
    println!();
    println!("\x1b[1mEXAMPLES:\x1b[0m");
    println!("  agent-browser skill install              # Install to user scope");
    println!("  agent-browser skill install --project    # Install to project scope");
    println!("  agent-browser skill install --force      # Overwrite existing");
    println!("  agent-browser skill uninstall --force    # Remove from user scope");
    println!("  agent-browser skill status               # Check installation status");
    println!("  agent-browser skill show                 # View skill documentation");
}
