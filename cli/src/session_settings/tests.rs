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
    assert!(!seeded(&[("AGENT_BROWSER_WEBGPU", "yes")]).webgpu());
    assert!(seeded(&[("AGENT_BROWSER_WEBGPU", "true")]).webgpu());
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
fn separator_only_allowed_domains_restrict_to_nothing() {
    let r = seeded(&[("AGENT_BROWSER_ALLOWED_DOMAINS", " , ")]);
    assert_eq!(r.security().allowed_domains, Some(vec![]));
    assert!(r.launch_options().restrict_webrtc);
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
