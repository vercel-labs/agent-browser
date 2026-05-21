//! Check remote browser providers: API key presence for Browserless,
//! Browserbase, Browser Use, Kernel, Steel, AgentCore (AWS), Appium for iOS, and
//! the AI Gateway chat key. Info-level unless the provider is selected
//! via `AGENT_BROWSER_PROVIDER`.

use std::env;

use super::helpers::which_exists;
use super::{Check, Status};

pub(super) fn check(checks: &mut Vec<Check>) {
    let category = "Providers";

    let active = env::var("AGENT_BROWSER_PROVIDER").ok();
    let normalized = active
        .as_ref()
        .map(|s| s.to_lowercase())
        .unwrap_or_default();

    let active_status = |provider: &str, ok: bool| -> Status {
        if normalized == provider {
            if ok {
                Status::Pass
            } else {
                Status::Fail
            }
        } else {
            Status::Info
        }
    };

    let providers: &[(&str, &[&str], &str)] = &[
        ("browserless", &["BROWSERLESS_API_KEY"], "Browserless"),
        ("browserbase", &["BROWSERBASE_API_KEY"], "Browserbase"),
        ("browseruse", &["BROWSER_USE_API_KEY"], "Browser Use"),
        ("kernel", &["KERNEL_API_KEY"], "Kernel"),
        ("steel", &["STEEL_API_KEY"], "Steel"),
    ];

    for (id, env_keys, label) in providers {
        let present = env_keys.iter().any(|k| env::var(k).is_ok());
        let provider_id = *id;
        let status = active_status(provider_id, present);
        let msg = if present {
            format!("{}: API key present", label)
        } else {
            format!("{}: {} not set", label, env_keys.join(" / "))
        };
        let mut check = Check::new(format!("providers.{}", provider_id), category, status, msg);
        if status == Status::Fail {
            check = check.with_fix(format!(
                "set {} (or unset AGENT_BROWSER_PROVIDER={})",
                env_keys.first().copied().unwrap_or(""),
                provider_id
            ));
        }
        checks.push(check);
    }

    let aws_present = env::var("AWS_ACCESS_KEY_ID").is_ok()
        || env::var("AWS_PROFILE").is_ok()
        || env::var("AWS_SESSION_TOKEN").is_ok();
    let agentcore_status = active_status("agentcore", aws_present);
    let mut agentcore_check = Check::new(
        "providers.agentcore",
        category,
        agentcore_status,
        if aws_present {
            "AgentCore: AWS credentials resolvable".to_string()
        } else {
            "AgentCore: no AWS credentials in env (AWS_ACCESS_KEY_ID / AWS_PROFILE)".to_string()
        },
    );
    if agentcore_status == Status::Fail {
        agentcore_check = agentcore_check
            .with_fix("export AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY or AWS_PROFILE");
    }
    checks.push(agentcore_check);

    if normalized == "ios" {
        if which_exists("appium") {
            checks.push(Check::new(
                "providers.ios",
                category,
                Status::Pass,
                "iOS: appium binary on PATH",
            ));
        } else {
            checks.push(
                Check::new(
                    "providers.ios",
                    category,
                    Status::Fail,
                    "iOS: appium binary not found on PATH",
                )
                .with_fix("npm install -g appium && appium driver install xcuitest"),
            );
        }
    }

    let chat_key_present = env::var("AI_GATEWAY_API_KEY").is_ok();
    if chat_key_present {
        checks.push(Check::new(
            "providers.chat",
            category,
            Status::Info,
            "AI_GATEWAY_API_KEY present (chat enabled)",
        ));
    } else {
        checks.push(
            Check::new(
                "providers.chat",
                category,
                Status::Info,
                "AI_GATEWAY_API_KEY not set (chat command disabled)",
            )
            .with_fix("export AI_GATEWAY_API_KEY=gw_..."),
        );
    }

    if let Some(active) = active {
        checks.push(Check::new(
            "providers.active",
            category,
            Status::Info,
            format!("AGENT_BROWSER_PROVIDER = {}", active),
        ));
    }
}
