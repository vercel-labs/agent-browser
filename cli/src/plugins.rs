//! External plugin protocol support.
//!
//! Plugins run out-of-process and communicate over a small stdio JSON protocol.
//! Core agent-browser keeps ownership of browser automation, policy checks, and
//! redaction-sensitive flows; credential plugins only resolve secrets on demand.

use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::AsyncWriteExt;

pub const PROTOCOL_VERSION: &str = "agent-browser.plugin.v1";
pub const TYPE_PLUGIN_MANIFEST: &str = "plugin.manifest";
pub const CAPABILITY_CREDENTIAL_READ: &str = "credential.read";
pub const CAPABILITY_BROWSER_PROVIDER: &str = "browser.provider";
pub const CAPABILITY_LAUNCH_MUTATE: &str = "launch.mutate";
pub const CAPABILITY_COMMAND_RUN: &str = "command.run";

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct PluginConfig {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub capabilities: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CredentialResolveRequest<'a> {
    pub profile_name: &'a str,
    pub item_ref: Option<&'a str>,
    pub url: Option<&'a str>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedCredential {
    pub username: String,
    pub password: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub otp: Option<String>,
    #[serde(default)]
    pub username_selector: Option<String>,
    #[serde(default)]
    pub password_selector: Option<String>,
    #[serde(default)]
    pub submit_selector: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BrowserProviderResult {
    #[serde(alias = "wsUrl", alias = "connectUrl", alias = "cdp_url")]
    pub cdp_url: String,
    #[serde(default)]
    pub direct_page: bool,
    #[serde(default)]
    pub cleanup: Option<serde_json::Value>,
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LaunchMutation {
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub extensions: Vec<String>,
    /// JavaScript source to register before page scripts run.
    #[serde(default)]
    pub init_scripts: Vec<String>,
    #[serde(default)]
    pub user_agent: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CredentialPluginResponse {
    protocol: String,
    success: bool,
    #[serde(default)]
    credential: Option<ResolvedCredential>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BrowserProviderPluginResponse {
    protocol: String,
    success: bool,
    #[serde(default)]
    browser: Option<BrowserProviderResult>,
    #[serde(default)]
    data: Option<BrowserProviderResult>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LaunchMutationPluginResponse {
    protocol: String,
    success: bool,
    #[serde(default)]
    launch: Option<LaunchMutation>,
    #[serde(default)]
    data: Option<LaunchMutation>,
}

#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
struct PluginManifest {
    name: Option<String>,
    capabilities: Vec<String>,
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PluginManifestResponse {
    protocol: String,
    success: bool,
    #[serde(default)]
    manifest: Option<PluginManifest>,
    #[serde(default)]
    data: Option<PluginManifest>,
}

pub fn plugins_from_env() -> Vec<PluginConfig> {
    std::env::var("AGENT_BROWSER_PLUGINS")
        .ok()
        .and_then(|raw| serde_json::from_str::<Vec<PluginConfig>>(&raw).ok())
        .unwrap_or_default()
}

pub fn find_plugin<'a>(plugins: &'a [PluginConfig], name: &str) -> Option<&'a PluginConfig> {
    plugins.iter().rev().find(|p| p.name == name)
}

pub fn plugin_policy_action(provider: &str, capability: &str) -> String {
    format!("plugin:{}:{}", provider, capability)
}

fn validate_plugin(plugin: &PluginConfig, capability: &str) -> Result<(), String> {
    if plugin.name.trim().is_empty() {
        return Err("Plugin entry is missing a name".to_string());
    }
    if plugin.command.trim().is_empty() {
        return Err(format!("Plugin '{}' is missing a command", plugin.name));
    }
    if !plugin.capabilities.iter().any(|c| c == capability) {
        return Err(format!(
            "Plugin '{}' does not declare required capability '{}'",
            plugin.name, capability
        ));
    }
    Ok(())
}

pub fn plugin_has_capability(plugin: &PluginConfig, capability: &str) -> bool {
    plugin.capabilities.iter().any(|c| c == capability)
}

pub fn resolved_plugins_with_capability<'a>(
    plugins: &'a [PluginConfig],
    capability: &str,
) -> Vec<&'a PluginConfig> {
    let mut resolved = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for plugin in plugins.iter().rev() {
        if seen.insert(plugin.name.as_str()) && plugin_has_capability(plugin, capability) {
            resolved.push(plugin);
        }
    }
    resolved.reverse();
    resolved
}

async fn invoke_plugin(
    plugin: &PluginConfig,
    request_type: &str,
    capability: &str,
    request: serde_json::Value,
    timeout_secs: u64,
    expose_plugin_error: bool,
) -> Result<serde_json::Value, String> {
    validate_plugin(plugin, capability)?;
    let payload = json!({
        "protocol": PROTOCOL_VERSION,
        "type": request_type,
        "capability": capability,
        "request": request,
    });
    invoke_plugin_process(plugin, payload, timeout_secs, expose_plugin_error).await
}

async fn invoke_plugin_process(
    plugin: &PluginConfig,
    payload: serde_json::Value,
    timeout_secs: u64,
    expose_plugin_error: bool,
) -> Result<serde_json::Value, String> {
    let mut command = tokio::process::Command::new(&plugin.command);
    command
        .args(&plugin.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);

    let mut child = command
        .spawn()
        .map_err(|e| format!("Failed to start plugin '{}': {}", plugin.name, e))?;

    if let Some(stdin) = child.stdin.as_mut() {
        let input = serde_json::to_vec(&payload)
            .map_err(|e| format!("Failed to encode plugin request: {}", e))?;
        stdin
            .write_all(&input)
            .await
            .map_err(|e| format!("Failed to write plugin request: {}", e))?;
    }
    drop(child.stdin.take());

    let output = tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        child.wait_with_output(),
    )
    .await
    .map_err(|_| format!("Plugin '{}' timed out", plugin.name))?
    .map_err(|e| format!("Plugin '{}' failed: {}", plugin.name, e))?;

    if !output.status.success() {
        return Err(format!("Plugin '{}' exited unsuccessfully", plugin.name));
    }

    let response: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|_| format!("Plugin '{}' returned invalid JSON", plugin.name))?;
    let protocol = response
        .get("protocol")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if protocol != PROTOCOL_VERSION {
        return Err(format!(
            "Plugin '{}' used unsupported protocol '{}'",
            plugin.name, protocol
        ));
    }
    if !response
        .get("success")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        if expose_plugin_error {
            let error = response
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("plugin returned success=false");
            return Err(format!("Plugin '{}' failed: {}", plugin.name, error));
        }
        return Err(format!("Plugin '{}' returned success=false", plugin.name));
    }

    Ok(response)
}

pub async fn resolve_credential_with_plugins(
    provider: &str,
    plugins: &[PluginConfig],
    request: CredentialResolveRequest<'_>,
) -> Result<ResolvedCredential, String> {
    let plugin = find_plugin(plugins, provider)
        .ok_or_else(|| format!("Credential plugin '{}' is not configured", provider))?;
    let response = invoke_plugin(
        plugin,
        "credential.resolve",
        CAPABILITY_CREDENTIAL_READ,
        json!({
            "profileName": request.profile_name,
            "itemRef": request.item_ref,
            "url": request.url,
        }),
        15,
        false,
    )
    .await?;
    let response: CredentialPluginResponse = serde_json::from_value(response)
        .map_err(|_| format!("Credential plugin '{}' returned invalid JSON", provider))?;
    if response.protocol != PROTOCOL_VERSION {
        return Err(format!(
            "Credential plugin '{}' used unsupported protocol '{}'",
            provider, response.protocol
        ));
    }
    if !response.success {
        return Err(format!(
            "Credential plugin '{}' could not resolve credentials",
            provider
        ));
    }
    let credential = response
        .credential
        .ok_or_else(|| format!("Credential plugin '{}' returned no credential", provider))?;
    if credential.username.is_empty() || credential.password.is_empty() {
        return Err(format!(
            "Credential plugin '{}' returned an incomplete credential",
            provider
        ));
    }
    Ok(credential)
}

pub async fn connect_browser_provider_with_plugins(
    provider: &str,
    plugins: &[PluginConfig],
    request: serde_json::Value,
) -> Result<BrowserProviderResult, String> {
    let plugin = find_plugin(plugins, provider)
        .ok_or_else(|| format!("Browser provider plugin '{}' is not configured", provider))?;
    let response = invoke_plugin(
        plugin,
        "browser.launch",
        CAPABILITY_BROWSER_PROVIDER,
        request,
        60,
        false,
    )
    .await?;
    let response: BrowserProviderPluginResponse =
        serde_json::from_value(response).map_err(|_| {
            format!(
                "Browser provider plugin '{}' returned invalid JSON",
                provider
            )
        })?;
    if response.protocol != PROTOCOL_VERSION {
        return Err(format!(
            "Browser provider plugin '{}' used unsupported protocol '{}'",
            provider, response.protocol
        ));
    }
    if !response.success {
        return Err(format!(
            "Browser provider plugin '{}' could not launch browser",
            provider
        ));
    }
    let browser = response
        .browser
        .or(response.data)
        .ok_or_else(|| format!("Browser provider plugin '{}' returned no browser", provider))?;
    if browser.cdp_url.is_empty() {
        return Err(format!(
            "Browser provider plugin '{}' returned an empty CDP URL",
            provider
        ));
    }
    Ok(browser)
}

pub async fn close_browser_provider_with_plugins(
    provider: &str,
    plugins: &[PluginConfig],
    cleanup: serde_json::Value,
) -> Result<(), String> {
    let Some(plugin) = find_plugin(plugins, provider) else {
        return Ok(());
    };
    if !plugin_has_capability(plugin, CAPABILITY_BROWSER_PROVIDER) {
        return Ok(());
    }
    let _ = invoke_plugin(
        plugin,
        "browser.close",
        CAPABILITY_BROWSER_PROVIDER,
        cleanup,
        15,
        false,
    )
    .await?;
    Ok(())
}

pub async fn launch_mutations_from_plugins(
    plugins: &[PluginConfig],
    request: serde_json::Value,
) -> Result<Vec<LaunchMutation>, String> {
    let mut mutations = Vec::new();
    for plugin in resolved_plugins_with_capability(plugins, CAPABILITY_LAUNCH_MUTATE) {
        let response = invoke_plugin(
            plugin,
            "launch.mutate",
            CAPABILITY_LAUNCH_MUTATE,
            request.clone(),
            15,
            false,
        )
        .await?;
        let response: LaunchMutationPluginResponse =
            serde_json::from_value(response).map_err(|_| {
                format!(
                    "Launch mutator plugin '{}' returned invalid JSON",
                    plugin.name
                )
            })?;
        if response.protocol != PROTOCOL_VERSION {
            return Err(format!(
                "Launch mutator plugin '{}' used unsupported protocol '{}'",
                plugin.name, response.protocol
            ));
        }
        if !response.success {
            return Err(format!(
                "Launch mutator plugin '{}' could not mutate launch options",
                plugin.name
            ));
        }
        if let Some(mutation) = response.launch.or(response.data) {
            mutations.push(mutation);
        }
    }
    Ok(mutations)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PluginSourceKind {
    Npm,
    Github,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PluginSource {
    kind: PluginSourceKind,
    reference: String,
    install_spec: String,
    source: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PluginConfigScope {
    Project,
    Global,
}

struct PluginAddOptions {
    reference: String,
    name: Option<String>,
    capabilities: Vec<String>,
    scope: PluginConfigScope,
    no_manifest: bool,
}

fn parse_plugin_source(reference: &str) -> Result<PluginSource, String> {
    let trimmed = reference.trim();
    if trimmed.is_empty() {
        return Err("plugin add requires a package or repository reference".to_string());
    }
    if trimmed.starts_with('@') {
        return Ok(PluginSource {
            kind: PluginSourceKind::Npm,
            reference: trimmed.to_string(),
            install_spec: trimmed.to_string(),
            source: format!("npm:{}", trimmed),
        });
    }

    let parts: Vec<&str> = trimmed.split('/').collect();
    if parts.len() == 2
        && !parts[0].is_empty()
        && !parts[1].is_empty()
        && !trimmed.starts_with('.')
        && !trimmed.starts_with('/')
        && !trimmed.contains(':')
    {
        return Ok(PluginSource {
            kind: PluginSourceKind::Github,
            reference: trimmed.to_string(),
            install_spec: format!("github:{}", trimmed),
            source: format!("github:{}", trimmed),
        });
    }

    Ok(PluginSource {
        kind: PluginSourceKind::Npm,
        reference: trimmed.to_string(),
        install_spec: trimmed.to_string(),
        source: format!("npm:{}", trimmed),
    })
}

fn parse_plugin_add_args(args: &[String]) -> Result<PluginAddOptions, String> {
    let mut reference = None;
    let mut name = None;
    let mut capabilities = Vec::new();
    let mut scope = PluginConfigScope::Project;
    let mut no_manifest = false;
    let mut i = 2;

    while i < args.len() {
        match args[i].as_str() {
            "--name" => {
                let Some(value) = args.get(i + 1) else {
                    return Err("plugin add --name requires a value".to_string());
                };
                name = Some(value.clone());
                i += 1;
            }
            "--capability" | "--capabilities" => {
                let Some(value) = args.get(i + 1) else {
                    return Err(format!("plugin add {} requires a value", args[i]));
                };
                capabilities.extend(
                    value
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(String::from),
                );
                i += 1;
            }
            "--global" => {
                scope = PluginConfigScope::Global;
            }
            "--project" => {
                scope = PluginConfigScope::Project;
            }
            "--no-manifest" => {
                no_manifest = true;
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag '{}' for plugin add", other));
            }
            value => {
                if reference.is_some() {
                    return Err(format!("unexpected argument '{}' for plugin add", value));
                }
                reference = Some(value.to_string());
            }
        }
        i += 1;
    }

    Ok(PluginAddOptions {
        reference: reference
            .ok_or_else(|| "plugin add requires a package or repository reference".to_string())?,
        name,
        capabilities,
        scope,
        no_manifest,
    })
}

async fn discover_plugin_manifest(plugin: &PluginConfig) -> Result<PluginManifest, String> {
    let payload = json!({
        "protocol": PROTOCOL_VERSION,
        "type": TYPE_PLUGIN_MANIFEST,
        "capability": TYPE_PLUGIN_MANIFEST,
        "request": {},
    });

    let response = invoke_plugin_process(plugin, payload, 60, true).await?;
    let response: PluginManifestResponse = serde_json::from_value(response)
        .map_err(|_| format!("Plugin '{}' returned invalid manifest JSON", plugin.name))?;
    if response.protocol != PROTOCOL_VERSION {
        return Err(format!(
            "Plugin '{}' used unsupported protocol '{}'",
            plugin.name, response.protocol
        ));
    }
    if !response.success {
        return Err(format!("Plugin '{}' returned success=false", plugin.name));
    }
    response
        .manifest
        .or(response.data)
        .ok_or_else(|| format!("Plugin '{}' returned no manifest", plugin.name))
}

fn derive_plugin_name(source: &PluginSource) -> String {
    let raw = match source.kind {
        PluginSourceKind::Npm => package_name_without_version(&source.reference),
        PluginSourceKind::Github => source
            .reference
            .rsplit('/')
            .next()
            .unwrap_or(&source.reference)
            .to_string(),
    };
    raw.strip_prefix("agent-browser-plugin-")
        .or_else(|| raw.strip_prefix("plugin-"))
        .unwrap_or(&raw)
        .to_string()
}

fn package_name_without_version(reference: &str) -> String {
    if reference.starts_with('@') {
        let mut parts = reference.splitn(2, '/');
        let scope = parts.next().unwrap_or_default();
        let name = parts.next().unwrap_or(scope);
        return name
            .rsplit_once('@')
            .map(|(pkg, _)| pkg)
            .unwrap_or(name)
            .to_string();
    }
    reference.split('@').next().unwrap_or(reference).to_string()
}

fn validate_plugin_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("plugin name cannot be empty".to_string());
    }
    if name.chars().any(|c| c.is_whitespace() || c == ':') {
        return Err("plugin name cannot contain whitespace or ':'".to_string());
    }
    Ok(())
}

fn config_path_for_scope(scope: &PluginConfigScope) -> Result<PathBuf, String> {
    match scope {
        PluginConfigScope::Project => Ok(PathBuf::from("agent-browser.json")),
        PluginConfigScope::Global => dirs::home_dir()
            .map(|d| d.join(".agent-browser").join("config.json"))
            .ok_or_else(|| "Could not determine home directory".to_string()),
    }
}

fn read_config_json(path: &Path) -> Result<serde_json::Value, String> {
    if !path.exists() {
        return Ok(json!({}));
    }
    let raw = fs::read_to_string(path)
        .map_err(|e| format!("Failed to read config {}: {}", path.display(), e))?;
    let value: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| format!("Config {} is invalid JSON: {}", path.display(), e))?;
    if !value.is_object() {
        return Err(format!("Config {} must be a JSON object", path.display()));
    }
    Ok(value)
}

fn write_config_json(path: &Path, value: &serde_json::Value) -> Result<(), String> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(|e| {
            format!(
                "Failed to create config directory {}: {}",
                parent.display(),
                e
            )
        })?;
    }
    let raw = serde_json::to_string_pretty(value)
        .map_err(|e| format!("Failed to encode config JSON: {}", e))?;
    fs::write(path, format!("{}\n", raw))
        .map_err(|e| format!("Failed to write config {}: {}", path.display(), e))
}

fn upsert_plugin_config(path: &Path, plugin: &PluginConfig) -> Result<(), String> {
    let mut value = read_config_json(path)?;
    let obj = value
        .as_object_mut()
        .ok_or_else(|| format!("Config {} must be a JSON object", path.display()))?;
    let plugins_value = obj
        .entry("plugins".to_string())
        .or_insert_with(|| json!([]));
    let plugins = plugins_value
        .as_array_mut()
        .ok_or_else(|| format!("Config {} field 'plugins' must be an array", path.display()))?;
    plugins.retain(|entry| {
        entry
            .get("name")
            .and_then(|v| v.as_str())
            .map(|name| name != plugin.name)
            .unwrap_or(true)
    });
    plugins.push(
        serde_json::to_value(plugin)
            .map_err(|e| format!("Failed to encode plugin config: {}", e))?,
    );
    write_config_json(path, &value)
}

fn build_plugin_config_from_add(
    source: &PluginSource,
    options: &PluginAddOptions,
    manifest: Option<PluginManifest>,
) -> Result<PluginConfig, String> {
    let manifest_name = manifest.as_ref().and_then(|m| m.name.clone());
    let name = options
        .name
        .clone()
        .or(manifest_name)
        .unwrap_or_else(|| derive_plugin_name(source));
    validate_plugin_name(&name)?;

    let capabilities = if options.capabilities.is_empty() {
        manifest.map(|m| m.capabilities).unwrap_or_default()
    } else {
        options.capabilities.clone()
    };
    if capabilities.is_empty() {
        return Err(
            "Plugin did not declare capabilities; rerun with --capability <name> or use a plugin that supports plugin.manifest"
                .to_string(),
        );
    }

    Ok(PluginConfig {
        name,
        command: "npx".to_string(),
        args: vec!["-y".to_string(), source.install_spec.clone()],
        capabilities,
        source: Some(source.source.clone()),
    })
}

fn print_plugin_added(plugin: &PluginConfig, path: &Path, json_output: bool) {
    if json_output {
        println!(
            "{}",
            json!({
                "success": true,
                "plugin": plugin,
                "configPath": path.display().to_string(),
            })
        );
        return;
    }

    println!("Added plugin '{}'", plugin.name);
    println!("Config: {}", path.display());
    if let Some(source) = &plugin.source {
        println!("Source: {}", source);
    }
    println!("Command: {} {}", plugin.command, plugin.args.join(" "));
    println!("Capabilities: {}", plugin.capabilities.join(", "));
}

fn add_plugin_command(args: &[String], json_output: bool) -> Result<(), String> {
    let options = parse_plugin_add_args(args)?;
    let source = parse_plugin_source(&options.reference)?;
    let provisional = PluginConfig {
        name: options
            .name
            .clone()
            .unwrap_or_else(|| derive_plugin_name(&source)),
        command: "npx".to_string(),
        args: vec!["-y".to_string(), source.install_spec.clone()],
        capabilities: Vec::new(),
        source: Some(source.source.clone()),
    };

    let manifest = if options.no_manifest {
        None
    } else {
        let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
        match rt.block_on(discover_plugin_manifest(&provisional)) {
            Ok(manifest) => Some(manifest),
            Err(e) if options.capabilities.is_empty() => {
                return Err(format!(
                    "{}. Rerun with --capability <name> to add the plugin without manifest discovery.",
                    e
                ));
            }
            Err(_) => None,
        }
    };

    let plugin = build_plugin_config_from_add(&source, &options, manifest)?;
    let path = config_path_for_scope(&options.scope)?;
    upsert_plugin_config(&path, &plugin)?;
    print_plugin_added(&plugin, &path, json_output);
    Ok(())
}

pub fn run_plugin_command(args: &[String], plugins: &[PluginConfig], json_output: bool) {
    let sub = args.get(1).map(|s| s.as_str()).unwrap_or("list");
    match sub {
        "list" => print_plugin_list(plugins, json_output),
        "add" | "install" => {
            if let Err(e) = add_plugin_command(args, json_output) {
                print_plugin_error(&e, json_output);
                std::process::exit(1);
            }
        }
        "show" => {
            let Some(name) = args.get(2) else {
                print_plugin_error("plugin show requires a plugin name", json_output);
                std::process::exit(1);
            };
            match find_plugin(plugins, name) {
                Some(plugin) => print_plugin(plugin, json_output),
                None => {
                    print_plugin_error(
                        &format!("Plugin '{}' is not configured", name),
                        json_output,
                    );
                    std::process::exit(1);
                }
            }
        }
        "run" => {
            let Some(name) = args.get(2) else {
                print_plugin_error("plugin run requires a plugin name", json_output);
                std::process::exit(1);
            };
            let Some(request_type) = args.get(3) else {
                print_plugin_error("plugin run requires a request type", json_output);
                std::process::exit(1);
            };
            let payload = match parse_run_payload(args) {
                Ok(payload) => payload,
                Err(e) => {
                    print_plugin_error(&e, json_output);
                    std::process::exit(1);
                }
            };
            let Some(plugin) = find_plugin(plugins, name) else {
                print_plugin_error(&format!("Plugin '{}' is not configured", name), json_output);
                std::process::exit(1);
            };
            if is_core_plugin_entrypoint(request_type) {
                print_plugin_error(
                    &format!(
                        "plugin run cannot invoke core plugin entrypoint '{}'; use the dedicated agent-browser command path",
                        request_type
                    ),
                    json_output,
                );
                std::process::exit(1);
            }
            let capability = if plugin_has_capability(plugin, request_type) {
                request_type.as_str()
            } else {
                CAPABILITY_COMMAND_RUN
            };
            let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
            match rt.block_on(invoke_plugin(
                plugin,
                request_type,
                capability,
                payload,
                60,
                true,
            )) {
                Ok(response) => println!("{}", response),
                Err(e) => {
                    print_plugin_error(&e, json_output);
                    std::process::exit(1);
                }
            }
        }
        _ => {
            print_plugin_error(
                &format!(
                    "Unknown plugin subcommand '{}'. Valid options: add, list, show, run",
                    sub
                ),
                json_output,
            );
            std::process::exit(1);
        }
    }
}

fn parse_run_payload(args: &[String]) -> Result<serde_json::Value, String> {
    let mut payload = json!({});
    let mut i = 4;
    while i < args.len() {
        match args[i].as_str() {
            "--payload" => {
                let Some(raw) = args.get(i + 1) else {
                    return Err("plugin run --payload requires JSON".to_string());
                };
                payload = serde_json::from_str(raw)
                    .map_err(|e| format!("plugin run --payload is invalid JSON: {}", e))?;
                i += 1;
            }
            other => {
                return Err(format!("unknown flag '{}' for plugin run", other));
            }
        }
        i += 1;
    }
    Ok(payload)
}

fn is_core_plugin_entrypoint(entrypoint: &str) -> bool {
    matches!(
        entrypoint,
        CAPABILITY_CREDENTIAL_READ
            | CAPABILITY_BROWSER_PROVIDER
            | CAPABILITY_LAUNCH_MUTATE
            | TYPE_PLUGIN_MANIFEST
            | "credential.resolve"
            | "browser.launch"
            | "browser.close"
    )
}

fn print_plugin_list(plugins: &[PluginConfig], json_output: bool) {
    if json_output {
        println!("{}", json!({ "plugins": plugins }));
        return;
    }
    if plugins.is_empty() {
        println!("No plugins configured");
        return;
    }
    for plugin in plugins {
        let capabilities = if plugin.capabilities.is_empty() {
            "(no capabilities)".to_string()
        } else {
            plugin.capabilities.join(", ")
        };
        println!("{}  {}", plugin.name, capabilities);
    }
}

fn print_plugin(plugin: &PluginConfig, json_output: bool) {
    if json_output {
        println!("{}", json!({ "plugin": plugin }));
        return;
    }
    println!("Name: {}", plugin.name);
    println!("Command: {}", plugin.command);
    if !plugin.args.is_empty() {
        println!("Args: {}", plugin.args.join(" "));
    }
    if plugin.capabilities.is_empty() {
        println!("Capabilities: (none)");
    } else {
        println!("Capabilities: {}", plugin.capabilities.join(", "));
    }
}

fn print_plugin_error(message: &str, json_output: bool) {
    if json_output {
        println!("{}", json!({ "success": false, "error": message }));
    } else {
        eprintln!("{}", message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_plugin_prefers_last_entry() {
        let plugins = vec![
            PluginConfig {
                name: "vault".to_string(),
                command: "first".to_string(),
                capabilities: vec![CAPABILITY_CREDENTIAL_READ.to_string()],
                ..PluginConfig::default()
            },
            PluginConfig {
                name: "vault".to_string(),
                command: "second".to_string(),
                capabilities: vec![CAPABILITY_CREDENTIAL_READ.to_string()],
                ..PluginConfig::default()
            },
        ];

        assert_eq!(find_plugin(&plugins, "vault").unwrap().command, "second");
    }

    #[test]
    fn resolved_plugins_with_capability_prefers_last_duplicate_name() {
        let plugins = vec![
            PluginConfig {
                name: "stealth".to_string(),
                command: "user-stealth".to_string(),
                capabilities: vec![CAPABILITY_LAUNCH_MUTATE.to_string()],
                ..PluginConfig::default()
            },
            PluginConfig {
                name: "captcha".to_string(),
                command: "captcha".to_string(),
                capabilities: vec![CAPABILITY_COMMAND_RUN.to_string()],
                ..PluginConfig::default()
            },
            PluginConfig {
                name: "stealth".to_string(),
                command: "project-stealth".to_string(),
                capabilities: vec![CAPABILITY_LAUNCH_MUTATE.to_string()],
                ..PluginConfig::default()
            },
        ];

        let resolved = resolved_plugins_with_capability(&plugins, CAPABILITY_LAUNCH_MUTATE);

        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].command, "project-stealth");
    }

    #[test]
    fn resolved_plugins_with_capability_later_duplicate_can_disable_capability() {
        let plugins = vec![
            PluginConfig {
                name: "stealth".to_string(),
                command: "user-stealth".to_string(),
                capabilities: vec![CAPABILITY_LAUNCH_MUTATE.to_string()],
                ..PluginConfig::default()
            },
            PluginConfig {
                name: "stealth".to_string(),
                command: "project-stealth".to_string(),
                capabilities: vec![CAPABILITY_COMMAND_RUN.to_string()],
                ..PluginConfig::default()
            },
        ];

        let resolved = resolved_plugins_with_capability(&plugins, CAPABILITY_LAUNCH_MUTATE);

        assert!(resolved.is_empty());
    }

    #[test]
    fn plugin_policy_action_is_capability_scoped() {
        assert_eq!(
            plugin_policy_action("onepassword", CAPABILITY_CREDENTIAL_READ),
            "plugin:onepassword:credential.read"
        );
    }

    #[test]
    fn plugin_run_rejects_core_entrypoints() {
        assert!(is_core_plugin_entrypoint(CAPABILITY_CREDENTIAL_READ));
        assert!(is_core_plugin_entrypoint(CAPABILITY_BROWSER_PROVIDER));
        assert!(is_core_plugin_entrypoint(CAPABILITY_LAUNCH_MUTATE));
        assert!(is_core_plugin_entrypoint("credential.resolve"));
        assert!(is_core_plugin_entrypoint("browser.launch"));
        assert!(is_core_plugin_entrypoint("browser.close"));
        assert!(is_core_plugin_entrypoint("launch.mutate"));
        assert!(is_core_plugin_entrypoint(TYPE_PLUGIN_MANIFEST));
        assert!(!is_core_plugin_entrypoint(CAPABILITY_COMMAND_RUN));
        assert!(!is_core_plugin_entrypoint("captcha.solve"));
    }

    #[test]
    fn plugin_add_source_detection_matches_reference_shape() {
        let npm = parse_plugin_source("agent-browser-plugin-captcha").unwrap();
        assert_eq!(npm.kind, PluginSourceKind::Npm);
        assert_eq!(npm.install_spec, "agent-browser-plugin-captcha");
        assert_eq!(npm.source, "npm:agent-browser-plugin-captcha");
        assert_eq!(derive_plugin_name(&npm), "captcha");

        let scoped = parse_plugin_source("@acme/agent-browser-plugin-vault").unwrap();
        assert_eq!(scoped.kind, PluginSourceKind::Npm);
        assert_eq!(scoped.install_spec, "@acme/agent-browser-plugin-vault");
        assert_eq!(scoped.source, "npm:@acme/agent-browser-plugin-vault");
        assert_eq!(derive_plugin_name(&scoped), "vault");

        let github = parse_plugin_source("vercel-labs/agent-browser-plugin-browserbox").unwrap();
        assert_eq!(github.kind, PluginSourceKind::Github);
        assert_eq!(
            github.install_spec,
            "github:vercel-labs/agent-browser-plugin-browserbox"
        );
        assert_eq!(
            github.source,
            "github:vercel-labs/agent-browser-plugin-browserbox"
        );
        assert_eq!(derive_plugin_name(&github), "browserbox");
    }

    #[test]
    fn plugin_add_builds_config_from_manual_capabilities() {
        let args = vec![
            "plugin".to_string(),
            "add".to_string(),
            "agent-browser-plugin-captcha".to_string(),
            "--capability".to_string(),
            "command.run".to_string(),
            "--capability".to_string(),
            "captcha.solve".to_string(),
            "--no-manifest".to_string(),
        ];
        let options = parse_plugin_add_args(&args).unwrap();
        let source = parse_plugin_source(&options.reference).unwrap();
        let plugin = build_plugin_config_from_add(&source, &options, None).unwrap();

        assert_eq!(plugin.name, "captcha");
        assert_eq!(plugin.command, "npx");
        assert_eq!(
            plugin.args,
            vec!["-y".to_string(), "agent-browser-plugin-captcha".to_string()]
        );
        assert_eq!(
            plugin.capabilities,
            vec!["command.run".to_string(), "captcha.solve".to_string()]
        );
        assert_eq!(
            plugin.source.as_deref(),
            Some("npm:agent-browser-plugin-captcha")
        );
    }

    #[test]
    fn plugin_config_upsert_replaces_existing_plugin() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent-browser.json");
        fs::write(
            &path,
            r#"{"headed":true,"plugins":[{"name":"captcha","command":"old","capabilities":["command.run"]}]}"#,
        )
        .unwrap();

        let plugin = PluginConfig {
            name: "captcha".to_string(),
            command: "npx".to_string(),
            args: vec!["-y".to_string(), "agent-browser-plugin-captcha".to_string()],
            capabilities: vec!["command.run".to_string(), "captcha.solve".to_string()],
            source: Some("npm:agent-browser-plugin-captcha".to_string()),
        };

        upsert_plugin_config(&path, &plugin).unwrap();
        let value: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["headed"], true);
        let plugins = value["plugins"].as_array().unwrap();
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0]["name"], "captcha");
        assert_eq!(plugins[0]["command"], "npx");
        assert_eq!(plugins[0]["source"], "npm:agent-browser-plugin-captcha");
        assert_eq!(plugins[0]["capabilities"][1], "captcha.solve");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn resolve_credential_uses_configured_stdio_plugin() {
        use crate::test_utils::EnvGuard;
        use std::os::unix::fs::PermissionsExt;

        let _guard = EnvGuard::new(&["AGENT_BROWSER_PLUGINS"]);
        let dir = tempfile::tempdir().unwrap();
        let plugin_path = dir.path().join("mock-credential-plugin");
        std::fs::write(
            &plugin_path,
            r#"#!/bin/sh
cat >/dev/null
printf '%s' '{"protocol":"agent-browser.plugin.v1","success":true,"credential":{"username":"user","password":"pass","url":"https://example.com/login"}}'
"#,
        )
        .unwrap();
        let mut perms = std::fs::metadata(&plugin_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&plugin_path, perms).unwrap();

        let registry = serde_json::to_string(&vec![PluginConfig {
            name: "mock".to_string(),
            command: plugin_path.to_string_lossy().to_string(),
            capabilities: vec![CAPABILITY_CREDENTIAL_READ.to_string()],
            ..PluginConfig::default()
        }])
        .unwrap();
        _guard.set("AGENT_BROWSER_PLUGINS", &registry);

        let plugins = plugins_from_env();
        let credential = resolve_credential_with_plugins(
            "mock",
            &plugins,
            CredentialResolveRequest {
                profile_name: "example",
                item_ref: Some("Example"),
                url: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(credential.username, "user");
        assert_eq!(credential.password, "pass");
        assert_eq!(credential.url.as_deref(), Some("https://example.com/login"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn credential_plugin_failure_does_not_echo_plugin_error() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let plugin_path = dir.path().join("mock-failing-credential-plugin");
        std::fs::write(
            &plugin_path,
            r#"#!/bin/sh
cat >/dev/null
printf '%s' '{"protocol":"agent-browser.plugin.v1","success":false,"error":"secret-token-value"}'
"#,
        )
        .unwrap();
        let mut perms = std::fs::metadata(&plugin_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&plugin_path, perms).unwrap();

        let plugins = vec![PluginConfig {
            name: "mock".to_string(),
            command: plugin_path.to_string_lossy().to_string(),
            capabilities: vec![CAPABILITY_CREDENTIAL_READ.to_string()],
            ..PluginConfig::default()
        }];

        let err = resolve_credential_with_plugins(
            "mock",
            &plugins,
            CredentialResolveRequest {
                profile_name: "example",
                item_ref: Some("Example"),
                url: None,
            },
        )
        .await
        .unwrap_err();

        assert!(err.contains("success=false"));
        assert!(!err.contains("secret-token-value"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn browser_provider_plugin_returns_cdp_connection() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let plugin_path = dir.path().join("mock-browser-plugin");
        std::fs::write(
            &plugin_path,
            r#"#!/bin/sh
cat >/dev/null
printf '%s' '{"protocol":"agent-browser.plugin.v1","success":true,"browser":{"cdpUrl":"ws://127.0.0.1:9222/devtools/browser/test","directPage":true,"metadata":{"sessionId":"s1"},"cleanup":{"sessionId":"s1"}}}'
"#,
        )
        .unwrap();
        let mut perms = std::fs::metadata(&plugin_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&plugin_path, perms).unwrap();

        let plugins = vec![PluginConfig {
            name: "browserbox".to_string(),
            command: plugin_path.to_string_lossy().to_string(),
            capabilities: vec![CAPABILITY_BROWSER_PROVIDER.to_string()],
            ..PluginConfig::default()
        }];

        let browser = connect_browser_provider_with_plugins("browserbox", &plugins, json!({}))
            .await
            .unwrap();

        assert_eq!(browser.cdp_url, "ws://127.0.0.1:9222/devtools/browser/test");
        assert!(browser.direct_page);
        assert_eq!(browser.metadata.unwrap()["sessionId"], "s1");
        assert_eq!(browser.cleanup.unwrap()["sessionId"], "s1");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn launch_mutator_plugin_returns_launch_changes() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let plugin_path = dir.path().join("mock-launch-plugin");
        std::fs::write(
            &plugin_path,
            r#"#!/bin/sh
cat >/dev/null
printf '%s' '{"protocol":"agent-browser.plugin.v1","success":true,"launch":{"args":["--disable-blink-features=AutomationControlled"],"extensions":["/tmp/ext"],"initScripts":["Object.defineProperty(navigator,\"webdriver\",{get:()=>undefined});"],"userAgent":"plugin-agent"}}'
"#,
        )
        .unwrap();
        let mut perms = std::fs::metadata(&plugin_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&plugin_path, perms).unwrap();

        let plugins = vec![PluginConfig {
            name: "stealth".to_string(),
            command: plugin_path.to_string_lossy().to_string(),
            capabilities: vec![CAPABILITY_LAUNCH_MUTATE.to_string()],
            ..PluginConfig::default()
        }];

        let mutations = launch_mutations_from_plugins(&plugins, json!({}))
            .await
            .unwrap();

        assert_eq!(mutations.len(), 1);
        assert_eq!(
            mutations[0].args,
            vec!["--disable-blink-features=AutomationControlled".to_string()]
        );
        assert_eq!(mutations[0].extensions, vec!["/tmp/ext".to_string()]);
        assert_eq!(mutations[0].user_agent.as_deref(), Some("plugin-agent"));
        assert_eq!(mutations[0].init_scripts.len(), 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timed_out_plugin_is_killed() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let marker_path = dir.path().join("plugin-finished");
        let plugin_path = dir.path().join("mock-slow-plugin");
        std::fs::write(
            &plugin_path,
            r#"#!/bin/sh
cat >/dev/null
sleep 2
printf done > "$1"
printf '%s' '{"protocol":"agent-browser.plugin.v1","success":true,"data":{}}'
"#,
        )
        .unwrap();
        let mut perms = std::fs::metadata(&plugin_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&plugin_path, perms).unwrap();

        let plugin = PluginConfig {
            name: "slow".to_string(),
            command: plugin_path.to_string_lossy().to_string(),
            args: vec![marker_path.to_string_lossy().to_string()],
            capabilities: vec![CAPABILITY_COMMAND_RUN.to_string()],
            ..PluginConfig::default()
        };

        let err = invoke_plugin(
            &plugin,
            "slow.run",
            CAPABILITY_COMMAND_RUN,
            json!({}),
            1,
            true,
        )
        .await
        .unwrap_err();

        assert!(err.contains("timed out"));
        tokio::time::sleep(std::time::Duration::from_millis(2_500)).await;
        assert!(!marker_path.exists());
    }
}
