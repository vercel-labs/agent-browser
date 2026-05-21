//! Probe reachability of the Chrome for Testing CDN, AI Gateway (if
//! configured), and the currently-selected provider endpoint. Each probe
//! has a 3-second timeout.

use std::env;
use std::time::{Duration, Instant};

use super::{Check, Status};

pub(super) fn check(checks: &mut Vec<Check>) {
    let category = "Network";

    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            checks.push(Check::new(
                "net.runtime",
                category,
                Status::Fail,
                format!("Could not start tokio runtime for probes: {}", e),
            ));
            return;
        }
    };

    let client = match reqwest::Client::builder()
        .user_agent(format!("agent-browser/{}", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(3))
        .connect_timeout(Duration::from_secs(3))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            checks.push(Check::new(
                "net.client",
                category,
                Status::Fail,
                format!("Could not build HTTP client: {}", e),
            ));
            return;
        }
    };

    let chrome_url =
        "https://googlechromelabs.github.io/chrome-for-testing/last-known-good-versions-with-downloads.json";
    probe_url(
        &rt,
        &client,
        checks,
        category,
        "net.chrome_cdn",
        chrome_url,
        "Chrome for Testing CDN",
    );

    if env::var("AI_GATEWAY_API_KEY").is_ok() {
        let url = env::var("AI_GATEWAY_URL")
            .unwrap_or_else(|_| "https://ai-gateway.vercel.sh".to_string());
        probe_url(
            &rt,
            &client,
            checks,
            category,
            "net.ai_gateway",
            &url,
            "AI Gateway",
        );
    }

    if let Ok(provider) = env::var("AGENT_BROWSER_PROVIDER") {
        let url: Option<String> = match provider.to_lowercase().as_str() {
            "browserbase" => Some("https://api.browserbase.com".to_string()),
            "browserless" => Some(
                env::var("BROWSERLESS_API_URL")
                    .unwrap_or_else(|_| "https://production-sfo.browserless.io".to_string()),
            ),
            "browseruse" | "browser-use" => Some("https://api.browser-use.com".to_string()),
            "kernel" => Some(
                env::var("KERNEL_ENDPOINT")
                    .unwrap_or_else(|_| "https://api.onkernel.com".to_string()),
            ),
            "steel" => Some("https://api.steel.dev".to_string()),
            _ => None,
        };
        if let Some(url) = url {
            probe_url(
                &rt,
                &client,
                checks,
                category,
                "net.provider",
                &url,
                &format!("Provider {}", provider),
            );
        }
    }
}

fn probe_url(
    rt: &tokio::runtime::Runtime,
    client: &reqwest::Client,
    checks: &mut Vec<Check>,
    category: &'static str,
    id: &'static str,
    url: &str,
    label: &str,
) {
    let started = Instant::now();
    let result = rt.block_on(async { client.head(url).send().await });
    let elapsed_ms = started.elapsed().as_millis();
    match result {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() || status.is_redirection() || status.as_u16() == 405 {
                checks.push(Check::new(
                    id,
                    category,
                    Status::Pass,
                    format!(
                        "{} reachable ({}ms, HTTP {})",
                        label,
                        elapsed_ms,
                        status.as_u16()
                    ),
                ));
            } else {
                checks.push(Check::new(
                    id,
                    category,
                    Status::Warn,
                    format!(
                        "{} returned HTTP {} after {}ms",
                        label,
                        status.as_u16(),
                        elapsed_ms
                    ),
                ));
            }
        }
        Err(e) => {
            checks.push(
                Check::new(
                    id,
                    category,
                    Status::Fail,
                    format!("{} unreachable after {}ms: {}", label, elapsed_ms, e),
                )
                .with_fix("check network connectivity / firewall / proxy settings"),
            );
        }
    }
}
