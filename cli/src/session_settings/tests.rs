use std::collections::{BTreeSet, HashMap};

use serde_json::{json, Value};

use super::running::SecurityView;
use super::*;
use crate::flags::{split_launch_args, try_parse_flags, Flags};
use crate::test_utils::EnvGuard;

/// Flags with no environment and an empty config file, so nothing is set.
fn blank_flags() -> Flags {
    let names: Vec<String> = std::env::vars()
        .map(|(name, _)| name)
        .filter(|name| name.starts_with("AGENT_BROWSER_"))
        .collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let guard = EnvGuard::new(&refs);
    for name in &refs {
        guard.remove(name);
    }
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.json");
    std::fs::write(&config, "{}").unwrap();
    let args = vec![
        "--config".to_string(),
        config.to_string_lossy().into_owned(),
    ];
    try_parse_flags(&args).unwrap()
}

/// Every option set to a distinct value.
pub(crate) fn full_flags() -> Flags {
    let mut f = blank_flags();
    f.headed = true;
    f.cli_headed = true;
    f.debug = true;
    f.executable_path = Some("/bin/chrome".to_string());
    f.cdp = Some("ws://127.0.0.1:9333/devtools".to_string());
    f.cdp_headers = Some(r#"{"Authorization":"Bearer x"}"#.to_string());
    f.extensions = vec!["/ext/a".to_string(), "/ext/b".to_string()];
    f.init_scripts = vec!["/init/a.js".to_string()];
    f.enable = vec!["react-devtools".to_string()];
    f.profile = Some("/profile".to_string());
    f.state = Some("/state.json".to_string());
    f.proxy = Some("http://user:pw@proxy.example:8080".to_string());
    f.proxy_bypass = Some("localhost".to_string());
    f.args = Some("--no-first-run,--window-size=1280,720".to_string());
    f.user_agent = Some("ua/1".to_string());
    f.provider = Some("browserless".to_string());
    f.ignore_https_errors = true;
    f.ca_cert = Some("/ca.pem".to_string());
    f.clear_ca_cert = true;
    f.allow_file_access = true;
    f.hide_scrollbars = false;
    f.cli_hide_scrollbars = true;
    f.webgpu = true;
    f.cli_webgpu = true;
    f.no_webmcp = true;
    f.cli_no_webmcp = true;
    f.no_xvfb = true;
    f.device = Some("iPhone 15".to_string());
    f.auto_connect = true;
    f.pin_tab = true;
    f.cli_pin_tab = true;
    f.restore = Some("work".to_string());
    f.restore_save = Some("always".to_string());
    f.restore_check_url = Some("https://x.test/".to_string());
    f.restore_check_text = Some("Welcome".to_string());
    f.restore_check_fn = Some("() => true".to_string());
    f.color_scheme = Some("dark".to_string());
    f.download_path = Some("/downloads".to_string());
    f.allowed_domains = Some(vec!["a.test".to_string(), "*.b.test".to_string()]);
    f.action_policy = Some("/policy.json".to_string());
    f.confirm_actions = Some("click,fill".to_string());
    f.engine = Some("chrome".to_string());
    f.idle_timeout = Some("120000".to_string());
    f.default_timeout = Some(7000);
    f.no_auto_dialog = true;
    f.plugins = vec![serde_json::from_value(json!({"name": "p", "path": "/p"})).unwrap()];
    f
}

fn set(patch: &Patch, key: Key) -> Value {
    match patch.get(key) {
        Some(Requested::Set(v)) => v.clone(),
        other => panic!("{:?} not set: {:?}", key, other),
    }
}

// ---- option table ----

#[test]
fn table_has_one_row_per_key_in_key_order() {
    assert_eq!(SPECS.len(), Key::ALL.len());
    for (row, key) in SPECS.iter().zip(Key::ALL) {
        assert_eq!(row.key, *key);
        assert_eq!(spec(*key).key, *key);
    }
}

#[test]
fn table_names_are_unique_kebab_case() {
    let mut seen = BTreeSet::new();
    for row in SPECS {
        assert!(seen.insert(row.name), "duplicate name {}", row.name);
        assert!(
            row.name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
            "{} is not kebab-case",
            row.name
        );
        assert!(row.flag.is_empty() || row.flag.starts_with("--"));
        assert_eq!(spec_by_name(row.name).map(|s| s.key), Some(row.key));
    }
}

#[test]
fn only_security_settings_are_secured() {
    let secured: Vec<Key> = SPECS
        .iter()
        .filter(|s| s.persist == Persist::Secured)
        .map(|s| s.key)
        .collect();
    assert_eq!(
        secured,
        vec![Key::AllowedDomains, Key::ActionPolicy, Key::ConfirmActions]
    );
}

#[test]
fn credentials_are_secret() {
    let secret: Vec<Key> = SPECS.iter().filter(|s| s.secret).map(|s| s.key).collect();
    assert_eq!(secret, vec![Key::CdpHeaders, Key::ProxyPassword]);
}

#[test]
fn launch_class_is_what_changes_the_browser() {
    for key in [Key::Profile, Key::Args, Key::Engine, Key::Proxy, Key::Cdp] {
        assert_eq!(spec(key).effect, Effect::Launch, "{:?}", key);
    }
    for key in [
        Key::DefaultTimeout,
        Key::IdleTimeout,
        Key::PinTab,
        Key::NoAutoDialog,
    ] {
        assert_eq!(spec(key).effect, Effect::Live, "{:?}", key);
    }
}

// ---- Patch::from_flags ----

#[test]
fn blank_flags_make_a_noop_patch() {
    let patch = Patch::from_flags(&blank_flags());
    assert!(patch.is_noop());
    assert_eq!(patch.to_wire(), None);
}

#[test]
fn full_flags_set_every_key() {
    let patch = Patch::from_flags(&full_flags());
    let present: BTreeSet<Key> = patch.iter().map(|(k, _)| k).collect();
    let all: BTreeSet<Key> = Key::ALL.iter().copied().collect();
    assert_eq!(
        present.difference(&all).count() + all.difference(&present).count(),
        0,
        "missing from the patch: {:?}",
        all.difference(&present).collect::<Vec<_>>()
    );
}

#[test]
fn every_value_has_the_shape_its_row_declares() {
    for (key, requested) in Patch::from_flags(&full_flags()).iter() {
        if let Requested::Set(v) = requested {
            assert!(spec(key).shape.accepts(v), "{:?} = {}", key, v);
        }
    }
}

/// Mirrors the `DaemonOptions` built in `main.rs`: each field that struct
/// forwards to the daemon is present in the patch with the same value.
#[test]
fn patch_carries_every_daemon_options_field() {
    let f = full_flags();
    let p = Patch::from_flags(&f);
    let parsed = parse_proxy(f.proxy.as_deref().unwrap());

    assert_eq!(set(&p, Key::Headed), json!(true)); // headed
    assert_eq!(set(&p, Key::Debug), json!(true)); // debug
    assert_eq!(set(&p, Key::ExecutablePath), json!("/bin/chrome")); // executable_path
    assert_eq!(set(&p, Key::Extensions), json!(f.extensions)); // extensions
    assert_eq!(set(&p, Key::InitScripts), json!(f.init_scripts)); // init_scripts
    assert_eq!(set(&p, Key::Enable), json!(f.enable)); // enable
    assert_eq!(
        set(&p, Key::Args),
        json!(split_launch_args(f.args.as_deref().unwrap()))
    ); // args
    assert_eq!(set(&p, Key::UserAgent), json!("ua/1")); // user_agent
    assert_eq!(set(&p, Key::Proxy), json!(parsed.server)); // proxy
    assert_eq!(set(&p, Key::ProxyBypass), json!("localhost")); // proxy_bypass
    assert_eq!(set(&p, Key::ProxyUsername), json!("user")); // proxy_username
    assert_eq!(set(&p, Key::ProxyPassword), json!("pw")); // proxy_password
    assert_eq!(set(&p, Key::IgnoreHttpsErrors), json!(true)); // ignore_https_errors
    assert_eq!(set(&p, Key::AllowFileAccess), json!(true)); // allow_file_access
    assert_eq!(set(&p, Key::HideScrollbars), json!(false)); // hide_scrollbars
    assert_eq!(set(&p, Key::Webgpu), json!(true)); // webgpu
    assert_eq!(set(&p, Key::Profile), json!("/profile")); // profile
    assert_eq!(set(&p, Key::StorageState), json!("/state.json")); // state
    assert_eq!(set(&p, Key::Provider), json!("browserless")); // provider
    assert_eq!(set(&p, Key::Device), json!("iPhone 15")); // device
    assert_eq!(set(&p, Key::RestoreKey), json!("work")); // session_name
    assert_eq!(set(&p, Key::RestoreSave), json!("always")); // restore_save
    assert_eq!(set(&p, Key::RestoreCheckUrl), json!("https://x.test/")); // restore_check_url
    assert_eq!(set(&p, Key::RestoreCheckText), json!("Welcome")); // restore_check_text
    assert_eq!(set(&p, Key::RestoreCheckFn), json!("() => true")); // restore_check_fn
    assert_eq!(set(&p, Key::DownloadPath), json!("/downloads")); // download_path
    assert_eq!(set(&p, Key::AllowedDomains), json!(["a.test", "*.b.test"])); // allowed_domains
    assert_eq!(set(&p, Key::ActionPolicy), json!("/policy.json")); // action_policy
    assert_eq!(set(&p, Key::ConfirmActions), json!("click,fill")); // confirm_actions
    assert_eq!(set(&p, Key::Engine), json!("chrome")); // engine
    assert_eq!(set(&p, Key::AutoConnect), json!(true)); // auto_connect
    assert_eq!(set(&p, Key::PinTab), json!(true)); // pin_tab
    assert_eq!(set(&p, Key::IdleTimeout), json!("120000")); // idle_timeout
    assert_eq!(set(&p, Key::DefaultTimeout), json!(7000)); // default_timeout
    assert_eq!(set(&p, Key::Cdp), json!("ws://127.0.0.1:9333/devtools")); // cdp
    assert_eq!(
        set(&p, Key::CdpHeaders),
        json!(r#"{"Authorization":"Bearer x"}"#)
    ); // cdp_headers
    assert_eq!(set(&p, Key::NoAutoDialog), json!(true)); // no_auto_dialog
    assert_eq!(
        set(&p, Key::Plugins),
        serde_json::to_value(&f.plugins).unwrap()
    ); // plugins
}

/// Mirrors the fields the launch envelope builders in `main.rs` attach.
#[test]
fn patch_carries_every_launch_envelope_field() {
    let f = full_flags();
    let p = Patch::from_flags(&f);

    // headless: !headed
    assert_eq!(set(&p, Key::Headed), json!(f.headed));
    // initScripts, enable
    assert_eq!(set(&p, Key::InitScripts), json!(["/init/a.js"]));
    assert_eq!(set(&p, Key::Enable), json!(["react-devtools"]));
    // webmcp: !no_webmcp, sent when set on the command line
    assert_eq!(set(&p, Key::Webmcp), json!(false));
    // allowedDomains, pinTab
    assert_eq!(set(&p, Key::AllowedDomains), json!(["a.test", "*.b.test"]));
    assert_eq!(set(&p, Key::PinTab), json!(true));
    // restoreKey, restoreSave, restoreCheck*
    assert_eq!(set(&p, Key::RestoreKey), json!("work"));
    // ignoreHTTPSErrors, caCert, clearCaCert
    assert_eq!(set(&p, Key::IgnoreHttpsErrors), json!(true));
    assert_eq!(set(&p, Key::CaCert), json!("/ca.pem"));
    assert_eq!(set(&p, Key::ClearCaCert), json!(true));
    // colorScheme, downloadPath
    assert_eq!(set(&p, Key::ColorScheme), json!("dark"));
    assert_eq!(set(&p, Key::DownloadPath), json!("/downloads"));
    // hideScrollbars, webgpu, noXvfb
    assert_eq!(set(&p, Key::HideScrollbars), json!(false));
    assert_eq!(set(&p, Key::Webgpu), json!(true));
    assert_eq!(set(&p, Key::NoXvfb), json!(true));
    // executablePath, profile, storageState, userAgent, args, extensions
    assert_eq!(set(&p, Key::ExecutablePath), json!("/bin/chrome"));
    assert_eq!(set(&p, Key::Profile), json!("/profile"));
    assert_eq!(set(&p, Key::StorageState), json!("/state.json"));
    assert_eq!(set(&p, Key::UserAgent), json!("ua/1"));
    assert_eq!(
        set(&p, Key::Args),
        json!(["--no-first-run", "--window-size=1280,720"])
    );
    assert_eq!(set(&p, Key::Extensions), json!(["/ext/a", "/ext/b"]));
    // proxy { server, username, password, bypass }
    assert_eq!(set(&p, Key::Proxy), json!("http://proxy.example:8080"));
    assert_eq!(set(&p, Key::ProxyUsername), json!("user"));
    assert_eq!(set(&p, Key::ProxyPassword), json!("pw"));
    assert_eq!(set(&p, Key::ProxyBypass), json!("localhost"));
    // engine, provider, autoConnect, cdpUrl, cdpHeaders
    assert_eq!(set(&p, Key::Engine), json!("chrome"));
    assert_eq!(set(&p, Key::Provider), json!("browserless"));
    assert_eq!(set(&p, Key::AutoConnect), json!(true));
    assert_eq!(set(&p, Key::Cdp), json!("ws://127.0.0.1:9333/devtools"));
}

#[test]
fn proxy_without_credentials_sets_no_credentials() {
    let mut f = blank_flags();
    f.proxy = Some("socks5://proxy.example:1080".to_string());
    let p = Patch::from_flags(&f);
    assert_eq!(set(&p, Key::Proxy), json!("socks5://proxy.example:1080"));
    assert!(p.get(Key::ProxyUsername).is_none());
    assert!(p.get(Key::ProxyPassword).is_none());
}

#[test]
fn obscura_clears_an_unset_proxy_bypass() {
    let mut f = blank_flags();
    f.engine = Some("obscura".to_string());
    assert_eq!(
        Patch::from_flags(&f).get(Key::ProxyBypass),
        Some(&Requested::Default)
    );
    f.engine = Some("chrome".to_string());
    assert_eq!(Patch::from_flags(&f).get(Key::ProxyBypass), None);
}

#[test]
fn restore_checks_clear_when_a_restore_key_is_set() {
    let mut f = blank_flags();
    f.restore = Some("work".to_string());
    let p = Patch::from_flags(&f);
    assert_eq!(set(&p, Key::RestoreSave), json!("auto"));
    assert_eq!(p.get(Key::RestoreCheckUrl), Some(&Requested::Default));
    assert_eq!(p.get(Key::RestoreCheckText), Some(&Requested::Default));
    assert_eq!(p.get(Key::RestoreCheckFn), Some(&Requested::Default));

    f.restore = None;
    f.session_name = Some("legacy".to_string());
    assert_eq!(
        set(&Patch::from_flags(&f), Key::RestoreKey),
        json!("legacy")
    );
}

#[test]
fn only_explicit_booleans_are_sent() {
    let mut f = blank_flags();
    // Defaults: nothing to say.
    assert!(Patch::from_flags(&f).is_noop());

    // Hiding scrollbars is the default; turning it off is news.
    f.hide_scrollbars = false;
    assert_eq!(
        set(&Patch::from_flags(&f), Key::HideScrollbars),
        json!(false)
    );

    // An explicit choice of the default is still a choice.
    f.hide_scrollbars = true;
    f.cli_hide_scrollbars = true;
    assert_eq!(
        set(&Patch::from_flags(&f), Key::HideScrollbars),
        json!(true)
    );

    // An explicit --no-pin-tab reaches a session that was pinned before.
    let mut g = blank_flags();
    g.cli_pin_tab = true;
    assert_eq!(set(&Patch::from_flags(&g), Key::PinTab), json!(false));

    // A bare false flag is absence, not a request.
    let mut h = blank_flags();
    h.ignore_https_errors = false;
    h.allow_file_access = false;
    assert!(Patch::from_flags(&h).is_noop());
}

// ---- wire ----

#[test]
fn wire_round_trips_the_full_patch() {
    let patch = Patch::from_flags(&full_flags());
    let wire = patch.to_wire().unwrap();
    assert_eq!(Patch::from_wire(Some(&wire)).unwrap(), patch);
}

#[test]
fn wire_uses_row_names_and_null_for_defaults() {
    let mut f = blank_flags();
    f.restore = Some("work".to_string());
    f.profile = Some("/p".to_string());
    let wire = Patch::from_flags(&f).to_wire().unwrap();
    assert_eq!(wire["profile"], json!("/p"));
    assert_eq!(wire["restore-key"], json!("work"));
    assert_eq!(wire["restore-check-url"], Value::Null);
}

#[test]
fn absent_wire_is_a_noop_patch() {
    assert!(Patch::from_wire(None).unwrap().is_noop());
    assert!(Patch::from_wire(Some(&json!({}))).unwrap().is_noop());
}

#[test]
fn wire_rejects_bad_input() {
    assert_eq!(
        Patch::from_wire(Some(&json!([1]))),
        Err(SettingsError::NotAnObject)
    );
    assert_eq!(
        Patch::from_wire(Some(&json!({"nope": 1}))),
        Err(SettingsError::UnknownKey("nope".to_string()))
    );
    assert!(matches!(
        Patch::from_wire(Some(&json!({"headed": "yes"}))),
        Err(SettingsError::Invalid(Key::Headed, _))
    ));
    assert!(matches!(
        Patch::from_wire(Some(&json!({"args": [1]}))),
        Err(SettingsError::Invalid(Key::Args, _))
    ));
    assert!(matches!(
        Patch::from_wire(Some(&json!({"default-timeout": -1}))),
        Err(SettingsError::Invalid(Key::DefaultTimeout, _))
    ));
}

// ---- Running views, seeded from a lookup ----

fn seeded(pairs: &[(&str, &str)]) -> Running {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    Running::from_lookup(|name| map.get(name).cloned())
}

#[test]
fn defaults_view() {
    let r = seeded(&[]);
    let o = r.launch_options();
    assert!(o.headless);
    assert!(o.args.is_empty());
    assert!(o.extensions.is_none());
    assert!(!o.allow_file_access);
    assert!(!o.ignore_https_errors);
    assert!(o.hide_scrollbars);
    assert!(!o.webgpu);
    assert!(o.webmcp);
    assert!(!o.no_xvfb);
    assert!(!o.restrict_webrtc);
    assert_eq!(r.connection(), Connection::Local);
    assert_eq!(r.default_timeout_ms(), 25_000);
    assert_eq!(r.engine(), None);
    assert!(!r.pin_tab());
    assert!(!r.no_auto_dialog());
    assert_eq!(r.security(), SecurityView::default());
    assert_eq!(r.restore().save, "auto");
    assert_eq!(r.restore().key, None);
    assert!(r.enable_features().is_empty());
    assert!(r.init_scripts().is_empty());
    assert_eq!(r.extensions_file_contents(), None);
}

#[test]
fn launch_options_view_reads_every_launch_variable() {
    let r = seeded(&[
        ("AGENT_BROWSER_HEADED", "true"),
        ("AGENT_BROWSER_EXECUTABLE_PATH", "/bin/chrome"),
        ("AGENT_BROWSER_PROXY", "http://proxy:1"),
        ("AGENT_BROWSER_PROXY_BYPASS", "localhost"),
        ("AGENT_BROWSER_PROXY_USERNAME", "u"),
        ("AGENT_BROWSER_PROXY_PASSWORD", "p"),
        ("AGENT_BROWSER_PROFILE", "/profile"),
        ("AGENT_BROWSER_ALLOW_FILE_ACCESS", "1"),
        (
            "AGENT_BROWSER_ARGS",
            "--disable-features=A,B,--no-first-run\n--window-size=1280,720",
        ),
        ("AGENT_BROWSER_EXTENSIONS", " /e/a, /e/b\n/e/c "),
        ("AGENT_BROWSER_STATE", "/state.json"),
        ("AGENT_BROWSER_USER_AGENT", "ua"),
        ("AGENT_BROWSER_IGNORE_HTTPS_ERRORS", "true"),
        ("AGENT_BROWSER_COLOR_SCHEME", "dark"),
        ("AGENT_BROWSER_DOWNLOAD_PATH", "/dl"),
        ("AGENT_BROWSER_HIDE_SCROLLBARS", "no"),
        ("AGENT_BROWSER_WEBGPU", "1"),
        ("AGENT_BROWSER_NO_WEBMCP", "yes"),
        ("AGENT_BROWSER_NO_XVFB", "true"),
        ("AGENT_BROWSER_ALLOWED_DOMAINS", "example.com"),
    ]);
    let o = r.launch_options();
    assert!(!o.headless);
    assert_eq!(o.executable_path.as_deref(), Some("/bin/chrome"));
    assert_eq!(o.proxy.as_deref(), Some("http://proxy:1"));
    assert_eq!(o.proxy_bypass.as_deref(), Some("localhost"));
    assert_eq!(o.proxy_username.as_deref(), Some("u"));
    assert_eq!(o.proxy_password.as_deref(), Some("p"));
    assert_eq!(o.profile.as_deref(), Some("/profile"));
    assert!(o.allow_file_access);
    assert_eq!(
        o.args,
        vec![
            "--disable-features=A,B",
            "--no-first-run",
            "--window-size=1280,720"
        ]
    );
    assert_eq!(
        o.extensions,
        Some(vec!["/e/a".into(), "/e/b".into(), "/e/c".into()])
    );
    assert_eq!(o.storage_state.as_deref(), Some("/state.json"));
    assert_eq!(o.user_agent.as_deref(), Some("ua"));
    assert!(o.ignore_https_errors);
    assert_eq!(o.color_scheme.as_deref(), Some("dark"));
    assert_eq!(o.download_path.as_deref(), Some("/dl"));
    assert!(!o.hide_scrollbars);
    assert!(o.webgpu);
    assert!(!o.webmcp);
    assert!(o.no_xvfb);
    assert!(o.restrict_webrtc);
    assert_eq!(o.ca_cert, None);
    assert_eq!(o.viewport_size, None);
    assert!(!o.use_real_keychain);
}

#[test]
fn boolean_spellings_match_each_variable() {
    // `1` or `true` only.
    assert!(!seeded(&[("AGENT_BROWSER_HEADED", "yes")]).headed());
    assert!(
        !seeded(&[("AGENT_BROWSER_WEBGPU", "yes")])
            .launch_options()
            .webgpu
    );
    assert!(
        seeded(&[("AGENT_BROWSER_WEBGPU", "true")])
            .launch_options()
            .webgpu
    );
    // `1`, `true` or `yes`.
    assert!(seeded(&[("AGENT_BROWSER_PIN_TAB", "yes")]).pin_tab());
    assert!(!seeded(&[("AGENT_BROWSER_PIN_TAB", "on")]).pin_tab());
    assert!(seeded(&[("AGENT_BROWSER_NO_AUTO_DIALOG", "true")]).no_auto_dialog());
    assert!(seeded(&[("AGENT_BROWSER_NO_WEBMCP", "0")]).webmcp());
    // Hiding scrollbars turns off for `0`, `false`, `no` and empty, any case.
    for off in ["0", "false", "No", "FALSE", ""] {
        assert!(!seeded(&[("AGENT_BROWSER_HIDE_SCROLLBARS", off)]).hide_scrollbars());
    }
    assert!(seeded(&[("AGENT_BROWSER_HIDE_SCROLLBARS", "1")]).hide_scrollbars());
    assert!(seeded(&[("AGENT_BROWSER_HIDE_SCROLLBARS", "anything")]).hide_scrollbars());
}

#[test]
fn connection_view_follows_precedence() {
    assert_eq!(
        seeded(&[("AGENT_BROWSER_CDP", "9222")]).connection(),
        Connection::Cdp("9222".into())
    );
    assert_eq!(
        seeded(&[("AGENT_BROWSER_AUTO_CONNECT", "0")]).connection(),
        Connection::AutoConnect,
        "presence alone turns auto-connect on"
    );
    assert_eq!(
        seeded(&[("AGENT_BROWSER_PROVIDER", "kernel")]).connection(),
        Connection::Provider("kernel".into())
    );
    assert_eq!(
        seeded(&[
            ("AGENT_BROWSER_PROVIDER", "kernel"),
            ("AGENT_BROWSER_AUTO_CONNECT", "1"),
            ("AGENT_BROWSER_CDP", "9222"),
        ])
        .connection(),
        Connection::Cdp("9222".into())
    );
    assert_eq!(
        seeded(&[
            ("AGENT_BROWSER_PROVIDER", "kernel"),
            ("AGENT_BROWSER_AUTO_CONNECT", "1"),
        ])
        .connection(),
        Connection::AutoConnect
    );
}

#[test]
fn timeout_views() {
    assert_eq!(
        seeded(&[("AGENT_BROWSER_DEFAULT_TIMEOUT", "7000")]).default_timeout_ms(),
        7000
    );
    assert_eq!(
        seeded(&[("AGENT_BROWSER_DEFAULT_TIMEOUT", "soon")]).default_timeout_ms(),
        25_000
    );
    assert_eq!(
        seeded(&[]).idle_timeout(),
        Some(IdleTimeout {
            ms: DEFAULT_IDLE_TIMEOUT_MS,
            is_default: true
        })
    );
    assert_eq!(
        seeded(&[("AGENT_BROWSER_IDLE_TIMEOUT_MS", " 5000 ")]).idle_timeout(),
        Some(IdleTimeout {
            ms: 5000,
            is_default: false
        })
    );
    assert_eq!(
        seeded(&[("AGENT_BROWSER_IDLE_TIMEOUT_MS", "0")]).idle_timeout(),
        None
    );
    assert_eq!(
        seeded(&[("AGENT_BROWSER_IDLE_TIMEOUT_MS", "x")])
            .idle_timeout()
            .map(|t| t.is_default),
        Some(true)
    );
}

#[test]
fn security_view() {
    let r = seeded(&[
        ("AGENT_BROWSER_ALLOWED_DOMAINS", " a.test , ,*.b.test "),
        ("AGENT_BROWSER_CONFIRM_ACTIONS", "click,fill"),
        ("AGENT_BROWSER_ACTION_POLICY", "/policy.json"),
    ]);
    assert_eq!(
        r.security(),
        SecurityView {
            allowed_domains: Some(vec!["a.test".into(), "*.b.test".into()]),
            action_policy_path: Some("/policy.json".into()),
            confirm_actions: Some("click,fill".into()),
        }
    );
}

#[test]
fn blank_allowed_domains_do_not_restrict() {
    let r = seeded(&[("AGENT_BROWSER_ALLOWED_DOMAINS", "  ")]);
    assert_eq!(r.security().allowed_domains, None);
    assert!(!r.launch_options().restrict_webrtc);
}

#[test]
fn separator_only_allowed_domains_seed_an_empty_list() {
    let r = seeded(&[("AGENT_BROWSER_ALLOWED_DOMAINS", " , ")]);
    assert_eq!(r.security().allowed_domains, Some(vec![]));
}

#[test]
fn headed_for_plugin_providers_accepts_any_value_but_off_spellings() {
    for on in ["1", "true", "yes", "on", "anything"] {
        assert!(
            seeded(&[("AGENT_BROWSER_HEADED", on)]).plugin_provider_headed(),
            "{on}"
        );
    }
    for off in ["0", "false", "No", ""] {
        assert!(
            !seeded(&[("AGENT_BROWSER_HEADED", off)]).plugin_provider_headed(),
            "{off}"
        );
    }
    assert!(!seeded(&[]).plugin_provider_headed());
    // The launch path keeps its own, stricter rule.
    assert!(!seeded(&[("AGENT_BROWSER_HEADED", "yes")]).headed());
}

#[test]
fn provider_view_ignores_connection_precedence() {
    let r = seeded(&[
        ("AGENT_BROWSER_PROVIDER", "kernel"),
        ("AGENT_BROWSER_CDP", "9222"),
    ]);
    assert_eq!(r.provider().as_deref(), Some("kernel"));
    assert_eq!(r.connection(), Connection::Cdp("9222".into()));
}

#[test]
fn legacy_policy_variable_applies_only_when_the_current_one_is_unset() {
    let legacy = seeded(&[("AGENT_BROWSER_POLICY", "/old.json")]);
    assert_eq!(
        legacy.security().action_policy_path.as_deref(),
        Some("/old.json")
    );
    let both = seeded(&[
        ("AGENT_BROWSER_POLICY", "/old.json"),
        ("AGENT_BROWSER_ACTION_POLICY", "/new.json"),
    ]);
    assert_eq!(
        both.security().action_policy_path.as_deref(),
        Some("/new.json")
    );
}

#[test]
fn restore_view() {
    let r = seeded(&[
        ("AGENT_BROWSER_SESSION_NAME", "work"),
        ("AGENT_BROWSER_RESTORE_SAVE", "never"),
        ("AGENT_BROWSER_RESTORE_CHECK_URL", "https://x.test/"),
        ("AGENT_BROWSER_RESTORE_CHECK_TEXT", "Hi"),
        ("AGENT_BROWSER_RESTORE_CHECK_FN", "() => 1"),
    ]);
    let v = r.restore();
    assert_eq!(v.key.as_deref(), Some("work"));
    assert_eq!(v.save, "never");
    assert_eq!(v.check_url.as_deref(), Some("https://x.test/"));
    assert_eq!(v.check_text.as_deref(), Some("Hi"));
    assert_eq!(v.check_fn.as_deref(), Some("() => 1"));
}

#[test]
fn script_lists_split_on_commas_and_newlines() {
    let r = seeded(&[
        ("AGENT_BROWSER_ENABLE", "react-devtools, other\n,"),
        ("AGENT_BROWSER_INIT_SCRIPTS", " /a.js,/b.js "),
    ]);
    assert_eq!(r.enable_features(), vec!["react-devtools", "other"]);
    assert_eq!(r.init_scripts(), vec!["/a.js", "/b.js"]);
}

#[test]
fn extensions_file_keeps_the_trimmed_original_value() {
    let r = seeded(&[("AGENT_BROWSER_EXTENSIONS", " /e/a, /e/b ")]);
    assert_eq!(r.extensions_file_contents(), Some("/e/a, /e/b"));
    assert_eq!(
        seeded(&[("AGENT_BROWSER_EXTENSIONS", "  ")]).extensions_file_contents(),
        None
    );
}

#[test]
fn cdp_headers_view_is_the_raw_value() {
    let raw = r#"{"Authorization":"Bearer x"}"#;
    assert_eq!(
        seeded(&[("AGENT_BROWSER_CDP_HEADERS", raw)])
            .cdp_headers()
            .as_deref(),
        Some(raw)
    );
}

#[test]
fn engine_view() {
    assert_eq!(
        seeded(&[("AGENT_BROWSER_ENGINE", "lightpanda")])
            .engine()
            .as_deref(),
        Some("lightpanda")
    );
}

#[test]
fn seeding_from_the_process_environment_reads_the_same_names() {
    let guard = EnvGuard::new(&["AGENT_BROWSER_PROFILE"]);
    guard.set("AGENT_BROWSER_PROFILE", "/from-env");
    assert_eq!(
        Running::from_env().launch_options().profile.as_deref(),
        Some("/from-env")
    );
}

#[test]
fn debug_output_never_contains_secrets() {
    let r = seeded(&[
        (
            "AGENT_BROWSER_PROXY",
            "http://user:hunter2@proxy.example:8080",
        ),
        ("AGENT_BROWSER_PROXY_PASSWORD", "hunter2"),
        ("AGENT_BROWSER_PROXY_USERNAME", "user"),
        (
            "AGENT_BROWSER_CDP_HEADERS",
            r#"{"Authorization":"Bearer s3cret"}"#,
        ),
    ]);
    let out = format!("{:?} {:#?}", r, r);
    for secret in ["hunter2", "s3cret"] {
        assert!(!out.contains(secret), "{secret} leaked: {out}");
    }
    assert!(out.contains("proxy.example"));

    let mut f = full_flags();
    f.proxy = Some("http://user:hunter2@proxy.example:8080".to_string());
    f.cdp_headers = Some(r#"{"Authorization":"Bearer s3cret"}"#.to_string());
    let patch = format!("{:?}", Patch::from_flags(&f));
    for secret in ["hunter2", "s3cret"] {
        assert!(!patch.contains(secret), "{secret} leaked: {patch}");
    }
    // The values themselves are untouched.
    assert_eq!(
        r.launch_options().proxy_password.as_deref(),
        Some("hunter2")
    );
}

// ---- Running::plan ----

fn patch_of(entries: &[(Key, Requested)]) -> Patch {
    Patch::from_entries(entries.iter().cloned())
}

fn is(key: Key, value: Value) -> (Key, Requested) {
    (key, Requested::Set(value))
}

fn running_with(entries: &[(Key, Value)]) -> Running {
    let mut r = Running::defaults();
    for (key, value) in entries {
        r.set(*key, value.clone());
    }
    r
}

fn changed_keys(plan: &super::plan::Plan) -> Vec<Key> {
    plan.changes.iter().map(|c| c.key).collect()
}

#[test]
fn plan_table_running_by_patch() {
    struct Case {
        name: &'static str,
        running: Vec<(Key, Value)>,
        patch: Vec<(Key, Requested)>,
        changed: Vec<Key>,
        relaunch: bool,
    }
    let cases = vec![
        Case {
            name: "an empty patch changes nothing",
            running: vec![(Key::Profile, json!("/a"))],
            patch: vec![],
            changed: vec![],
            relaunch: false,
        },
        Case {
            name: "a key the patch does not name is left alone",
            running: vec![(Key::Profile, json!("/a")), (Key::Headed, json!(true))],
            patch: vec![is(Key::Args, json!(["--x"]))],
            changed: vec![Key::Args],
            relaunch: true,
        },
        Case {
            name: "equal value is free",
            running: vec![(Key::Profile, json!("/a"))],
            patch: vec![is(Key::Profile, json!("/a"))],
            changed: vec![],
            relaunch: false,
        },
        Case {
            name: "a different launch value relaunches",
            running: vec![(Key::Profile, json!("/a"))],
            patch: vec![is(Key::Profile, json!("/b"))],
            changed: vec![Key::Profile],
            relaunch: true,
        },
        Case {
            name: "a live value applies without a relaunch",
            running: vec![(Key::DefaultTimeout, json!(5000))],
            patch: vec![is(Key::DefaultTimeout, json!(9000))],
            changed: vec![Key::DefaultTimeout],
            relaunch: false,
        },
        Case {
            name: "the default is equal to nothing running",
            running: vec![],
            patch: vec![
                is(Key::Headed, json!(false)),
                is(Key::HideScrollbars, json!(true)),
                is(Key::Webmcp, json!(true)),
                is(Key::RestoreSave, json!("auto")),
            ],
            changed: vec![],
            relaunch: false,
        },
        Case {
            name: "turning a default-true setting off is a change",
            running: vec![],
            patch: vec![is(Key::HideScrollbars, json!(false))],
            changed: vec![Key::HideScrollbars],
            relaunch: true,
        },
        Case {
            name: "an explicit reset of a set value is a change",
            running: vec![(Key::ProxyBypass, json!("localhost"))],
            patch: vec![(Key::ProxyBypass, Requested::Default)],
            changed: vec![Key::ProxyBypass],
            relaunch: true,
        },
        Case {
            name: "an explicit reset of an unset value is free",
            running: vec![],
            patch: vec![(Key::ProxyBypass, Requested::Default)],
            changed: vec![],
            relaunch: false,
        },
        Case {
            name: "live and launch changes together relaunch once",
            running: vec![
                (Key::Profile, json!("/a")),
                (Key::NoAutoDialog, json!(false)),
            ],
            patch: vec![
                is(Key::Profile, json!("/b")),
                is(Key::NoAutoDialog, json!(true)),
            ],
            changed: vec![Key::Profile, Key::NoAutoDialog],
            relaunch: true,
        },
        Case {
            name: "security settings follow their effect class",
            running: vec![],
            patch: vec![
                is(Key::AllowedDomains, json!(["a.test"])),
                is(Key::ActionPolicy, json!("/policy.json")),
                is(Key::ConfirmActions, json!("click")),
            ],
            changed: vec![Key::AllowedDomains, Key::ActionPolicy, Key::ConfirmActions],
            relaunch: true,
        },
        Case {
            name: "only live changes leave the browser alone",
            running: vec![],
            patch: vec![
                is(Key::ActionPolicy, json!("/policy.json")),
                is(Key::ConfirmActions, json!("click")),
                is(Key::IdleTimeout, json!("60000")),
                is(Key::PinTab, json!(true)),
            ],
            changed: vec![
                Key::ActionPolicy,
                Key::ConfirmActions,
                Key::IdleTimeout,
                Key::PinTab,
            ],
            relaunch: false,
        },
    ];
    for case in cases {
        let plan = running_with(&case.running).plan(&patch_of(&case.patch));
        assert_eq!(changed_keys(&plan), case.changed, "{}", case.name);
        assert_eq!(plan.relaunch, case.relaunch, "{}", case.name);
        assert_eq!(plan.is_empty(), case.changed.is_empty() && !case.relaunch);
    }
}

#[test]
fn plan_is_pure_and_next_holds_the_requested_values() {
    let running = running_with(&[(Key::Profile, json!("/a")), (Key::Args, json!(["--x"]))]);
    let before = running.clone();
    let plan = running.plan(&patch_of(&[
        is(Key::Profile, json!("/b")),
        (Key::Args, Requested::Default),
    ]));

    assert_eq!(
        running, before,
        "planning must not touch the running values"
    );
    assert_eq!(plan.next.get(Key::Profile), Some(&json!("/b")));
    assert_eq!(plan.next.get(Key::Args), None);
    assert_eq!(
        plan.changes[0],
        super::plan::Change {
            key: Key::Profile,
            requested: Requested::Set(json!("/b")),
            running: Some(json!("/a")),
        }
    );
}

#[test]
fn a_forced_relaunch_needs_no_change() {
    let mut flags = blank_flags();
    flags.relaunch = true;
    let patch = Patch::from_flags(&flags);
    assert!(!patch.is_noop());

    let plan = running_with(&[(Key::Profile, json!("/a"))]).plan(&patch);
    assert!(plan.changes.is_empty());
    assert!(plan.relaunch);
    assert!(!plan.is_empty());

    let wire = patch.to_wire().unwrap();
    assert_eq!(wire, json!({"relaunch": true}));
    assert_eq!(Patch::from_wire(Some(&wire)).unwrap(), patch);
}

#[test]
fn a_connection_target_displaces_the_others() {
    let running = running_with(&[
        (Key::Cdp, json!("9222")),
        (Key::CdpHeaders, json!(r#"{"Authorization":"x"}"#)),
    ]);
    let plan = running.plan(&patch_of(&[is(Key::Provider, json!("browserless"))]));

    assert_eq!(
        changed_keys(&plan),
        vec![Key::Provider, Key::Cdp, Key::CdpHeaders]
    );
    assert!(plan.relaunch);
    assert_eq!(
        plan.next.connection(),
        Connection::Provider("browserless".to_string())
    );
    assert_eq!(plan.next.get(Key::CdpHeaders), None);
}

#[test]
fn no_ca_cert_drops_the_trust_in_effect() {
    let running = running_with(&[(Key::CaCert, json!("/ca.pem"))]);
    let plan = running.plan(&patch_of(&[is(Key::ClearCaCert, json!(true))]));
    assert_eq!(changed_keys(&plan), vec![Key::ClearCaCert]);
    assert!(plan.relaunch);
    assert_eq!(plan.next.get(Key::CaCert), None);
    assert_eq!(
        plan.next.get(Key::ClearCaCert),
        None,
        "a one-shot is not kept"
    );

    let none_in_effect = Running::defaults().plan(&patch_of(&[is(Key::ClearCaCert, json!(true))]));
    assert!(none_in_effect.is_empty());
}

#[test]
fn planning_keeps_the_seed_shims_in_step() {
    let plan = Running::defaults().plan(&patch_of(&[
        is(Key::Extensions, json!(["/a", "/b"])),
        is(Key::Headed, json!(true)),
    ]));
    assert_eq!(plan.next.extensions_file_contents(), Some("/a,/b"));
    assert!(plan.next.plugin_provider_headed());
}

#[test]
fn changes_report_redacted_values() {
    let running = running_with(&[(Key::ProxyPassword, json!("old-secret"))]);
    let plan = running.plan(&patch_of(&[
        is(Key::ProxyPassword, json!("new-secret")),
        is(Key::CdpHeaders, json!(r#"{"Authorization":"Bearer t"}"#)),
        is(Key::Proxy, json!("http://user:pw@proxy.example:8080")),
    ]));

    let reported: Vec<Value> = plan.changes.iter().map(|c| c.to_json(true)).collect();
    let text = serde_json::to_string(&reported).unwrap();
    for secret in ["old-secret", "new-secret", "Bearer t", "user:pw"] {
        assert!(!text.contains(secret), "{secret} leaked: {text}");
    }
    let password = reported
        .iter()
        .find(|c| c["option"] == "proxy-password")
        .unwrap();
    assert_eq!(password["requested"], json!("<redacted>"));
    assert_eq!(password["running"], json!("<redacted>"));
    assert_eq!(password["effect"], json!("relaunch"));
    assert_eq!(password["applied"], json!(true));
}

#[test]
fn change_json_names_the_class_and_whether_it_took_effect() {
    let plan = Running::defaults().plan(&patch_of(&[
        is(Key::Profile, json!("/b")),
        is(Key::DefaultTimeout, json!(9000)),
    ]));
    let launch = plan.changes[0].to_json(false);
    assert_eq!(launch["option"], json!("profile"));
    assert_eq!(launch["requested"], json!("/b"));
    assert_eq!(launch["running"], Value::Null);
    assert_eq!(launch["effect"], json!("relaunch"));
    assert_eq!(launch["applied"], json!(false));
    let live = plan.changes[1].to_json(true);
    assert_eq!(live["effect"], json!("live"));
}

#[test]
fn launch_identity_follows_launch_settings_only() {
    let base = running_with(&[(Key::Profile, json!("/a"))]);
    let live_change = base
        .plan(&patch_of(&[is(Key::DefaultTimeout, json!(9000))]))
        .next;
    let launch_change = base.plan(&patch_of(&[is(Key::Profile, json!("/b"))])).next;

    assert_eq!(base.launch_identity(), live_change.launch_identity());
    assert_ne!(base.launch_identity(), launch_change.launch_identity());
}

#[test]
fn a_session_started_from_the_same_settings_plans_nothing() {
    let mut flags = blank_flags();
    flags.profile = Some("/work/profile".to_string());
    flags.args = Some("--lang=en-US,--no-first-run".to_string());
    flags.headed = true;
    flags.cli_headed = true;
    flags.default_timeout = Some(7000);
    flags.idle_timeout = Some("120000".to_string());

    let started = seeded(&[
        ("AGENT_BROWSER_PROFILE", "/work/profile"),
        ("AGENT_BROWSER_ARGS", "--lang=en-US,--no-first-run"),
        ("AGENT_BROWSER_HEADED", "1"),
        ("AGENT_BROWSER_DEFAULT_TIMEOUT", "7000"),
        ("AGENT_BROWSER_IDLE_TIMEOUT_MS", "120000"),
    ]);

    assert!(started.plan(&Patch::from_flags(&flags)).is_empty());
}

#[test]
fn a_value_marked_changed_in_place_is_a_change_even_when_equal() {
    let running = running_with(&[(Key::CaCert, json!("/ca.pem"))]);
    let mut patch = patch_of(&[is(Key::CaCert, json!("/ca.pem"))]);
    assert!(running.plan(&patch).is_empty());

    patch.mark_changed_in_place(Key::CaCert);
    let plan = running.plan(&patch);
    assert_eq!(changed_keys(&plan), vec![Key::CaCert]);
    assert!(plan.relaunch);
}

// ---- review fixes ----

#[test]
fn cdp_headers_do_not_follow_the_session_to_another_endpoint() {
    let running = running_with(&[
        (Key::Cdp, json!("wss://a.example/cdp")),
        (Key::CdpHeaders, json!(r#"{"Authorization":"Bearer a"}"#)),
    ]);

    let moved = running.plan(&patch_of(&[is(Key::Cdp, json!("wss://b.example/cdp"))]));
    assert_eq!(changed_keys(&moved), vec![Key::Cdp, Key::CdpHeaders]);
    assert_eq!(moved.next.cdp_headers(), None);

    let same = running.plan(&patch_of(&[is(Key::Cdp, json!("wss://a.example/cdp"))]));
    assert!(same.is_empty());
    assert!(same.next.cdp_headers().is_some());

    let with_headers = running.plan(&patch_of(&[
        is(Key::Cdp, json!("wss://b.example/cdp")),
        is(Key::CdpHeaders, json!(r#"{"Authorization":"Bearer b"}"#)),
    ]));
    assert_eq!(
        with_headers.next.cdp_headers().as_deref(),
        Some(r#"{"Authorization":"Bearer b"}"#)
    );
}

#[test]
fn cdp_urls_are_reported_without_userinfo_or_query() {
    let url = "wss://user:pw@host.example:9222/devtools/browser/abc?token=SECRET#frag";
    let plan = Running::defaults().plan(&patch_of(&[is(Key::Cdp, json!(url))]));
    let reported = plan.changes[0].to_json(true);
    assert_eq!(
        reported["requested"],
        json!("wss://host.example:9222/devtools/browser/abc?<redacted>")
    );
    for leaked in ["SECRET", "user:pw", "frag"] {
        assert!(!reported.to_string().contains(leaked), "{reported}");
    }
    assert!(!format!("{:?}", plan.next).contains("SECRET"));
    assert!(!format!("{:?}", patch_of(&[is(Key::Cdp, json!(url))])).contains("SECRET"));
    assert_eq!(super::spec::redact_url("9222"), "9222");
    assert_eq!(
        super::spec::redact_url("http://127.0.0.1:9222/json/version"),
        "http://127.0.0.1:9222/json/version"
    );
}

#[test]
fn storage_state_displaces_an_inherited_profile_and_extensions() {
    let running = running_with(&[
        (Key::Profile, json!("/p")),
        (Key::Extensions, json!(["/e"])),
    ]);
    let plan = running.plan(&patch_of(&[is(Key::StorageState, json!("/s.json"))]));
    assert_eq!(
        changed_keys(&plan),
        vec![Key::StorageState, Key::Profile, Key::Extensions]
    );
    assert_eq!(plan.next.get(Key::Profile), None);
    assert_eq!(plan.next.get(Key::Extensions), None);
    assert!(plan.relaunch);

    // And the other way round: the one set last wins.
    let back = plan.next.plan(&patch_of(&[is(Key::Profile, json!("/p"))]));
    assert_eq!(back.next.get(Key::StorageState), None);
}

#[test]
fn a_conflict_inside_one_patch_is_left_for_the_launch_to_reject() {
    let plan = Running::defaults().plan(&patch_of(&[
        is(Key::Profile, json!("/p")),
        is(Key::StorageState, json!("/s.json")),
    ]));
    assert_eq!(plan.next.get(Key::Profile), Some(&json!("/p")));
    assert_eq!(plan.next.get(Key::StorageState), Some(&json!("/s.json")));
}

#[test]
fn security_settings_cannot_be_cleared_with_a_null() {
    for name in ["allowed-domains", "action-policy", "confirm-actions"] {
        let wire = json!({ name: null });
        assert!(
            matches!(
                Patch::from_wire(Some(&wire)),
                Err(SettingsError::Invalid(_, _))
            ),
            "{name}"
        );
    }
    assert!(Patch::from_wire(Some(&json!({"proxy-bypass": null}))).is_ok());
}

#[test]
fn a_patch_without_launch_settings_keeps_only_live_ones() {
    let patch = patch_of(&[
        is(Key::Profile, json!("/p")),
        is(Key::DefaultTimeout, json!(5000)),
    ]);
    let live = patch.without_launch_settings();
    assert!(live.get(Key::Profile).is_none());
    assert!(live.get(Key::DefaultTimeout).is_some());
}

#[test]
fn local_only_settings_are_ignored_for_external_sessions() {
    for target in [
        (Key::Cdp, json!("ws://127.0.0.1:9999/devtools/browser/x")),
        (Key::Provider, json!("browserbase")),
        (Key::AutoConnect, json!(true)),
    ] {
        let running = running_with(&[target]);
        let plan = running.plan(&patch_of(&[
            is(Key::Args, json!(["--x"])),
            is(Key::Profile, json!("/p")),
            is(Key::ExecutablePath, json!("/chrome")),
            is(Key::Extensions, json!(["/e"])),
            is(Key::Proxy, json!("http://proxy:1")),
            is(Key::UserAgent, json!("ua")),
            is(Key::IgnoreHttpsErrors, json!(true)),
        ]));

        // Only the setting that also applies to an external browser changes.
        assert_eq!(changed_keys(&plan), vec![Key::IgnoreHttpsErrors]);
        let ignored: Vec<Key> = plan.ignored.iter().map(|c| c.key).collect();
        assert_eq!(
            ignored,
            vec![
                Key::Profile,
                Key::Args,
                Key::ExecutablePath,
                Key::Extensions,
                Key::Proxy,
                Key::UserAgent
            ]
        );
        assert_eq!(plan.next.get(Key::Args), None);
        assert!(plan.ignored[0].to_ignored_json()["reason"].is_string());

        let only_local = running.plan(&patch_of(&[is(Key::Args, json!(["--x"]))]));
        assert!(
            !only_local.relaunch,
            "env args never start a new cloud session"
        );
        assert!(only_local.is_empty());
    }
}

#[test]
fn local_only_settings_apply_to_a_local_session() {
    let plan = Running::defaults().plan(&patch_of(&[is(Key::Args, json!(["--x"]))]));
    assert!(plan.relaunch && plan.ignored.is_empty());

    // A target set by the same patch makes the local-only keys inert.
    let plan = Running::defaults().plan(&patch_of(&[
        is(Key::Provider, json!("browserbase")),
        is(Key::Args, json!(["--x"])),
    ]));
    assert_eq!(plan.ignored.len(), 1);
}

#[test]
fn no_xvfb_can_be_turned_off() {
    let mut flags = blank_flags();
    flags.no_xvfb_set = true;
    flags.no_xvfb = false;
    let patch = Patch::from_flags(&flags);
    assert_eq!(patch.get(Key::NoXvfb), Some(&Requested::Set(json!(false))));

    let running = running_with(&[(Key::NoXvfb, json!(true))]);
    assert_eq!(changed_keys(&running.plan(&patch)), vec![Key::NoXvfb]);
    assert!(Patch::from_flags(&blank_flags()).get(Key::NoXvfb).is_none());
}

#[test]
fn inherited_local_settings_are_dropped_when_the_session_goes_external() {
    let local = [
        (Key::StorageState, json!("/s.json")),
        (Key::Profile, json!("/p")),
        (Key::Extensions, json!(["/e"])),
        (Key::Proxy, json!("http://proxy:1")),
        (Key::ProxyUsername, json!("user")),
        (Key::ProxyPassword, json!("pw")),
        (Key::ExecutablePath, json!("/chrome")),
        (Key::Args, json!(["--x"])),
    ];
    for target in [
        (Key::Cdp, json!("ws://127.0.0.1:9999/devtools/browser/x")),
        (Key::Provider, json!("browserbase")),
        (Key::AutoConnect, json!(true)),
    ] {
        let running = running_with(&local);
        let plan = running.plan(&patch_of(&[is(target.0, target.1.clone())]));
        for (key, _) in &local {
            assert_eq!(plan.next.get(*key), None, "{key:?} survives {:?}", target.0);
        }
        let dropped: Vec<Key> = plan.ignored.iter().map(|c| c.key).collect();
        assert!(dropped.contains(&Key::StorageState) && dropped.contains(&Key::ProxyPassword));
        // Switching target relaunches once; dropping the values adds nothing.
        assert_eq!(changed_keys(&plan), vec![target.0]);
    }
}

#[test]
fn local_settings_are_kept_while_the_session_stays_local() {
    let running = running_with(&[
        (Key::StorageState, json!("/s.json")),
        (Key::ProxyUsername, json!("user")),
    ]);
    let plan = running.plan(&patch_of(&[is(Key::DefaultTimeout, json!(5000))]));
    assert_eq!(plan.next.get(Key::StorageState), Some(&json!("/s.json")));
    assert_eq!(plan.next.get(Key::ProxyUsername), Some(&json!("user")));
    assert!(plan.ignored.is_empty());
}

#[test]
fn an_empty_provider_resets_the_session_to_local() {
    let running = running_with(&[(Key::Cdp, json!("9222"))]);
    let plan = running.plan(&patch_of(&[is(Key::Provider, json!(""))]));
    assert_eq!(plan.next.connection(), Connection::Local);
    assert!(plan.relaunch);
}
