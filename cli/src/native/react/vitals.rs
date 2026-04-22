//! Core Web Vitals + React hydration timing report.
//!
//! Universal web-standard metrics (LCP/CLS/TTFB/FCP/INP) via PerformanceObserver
//! and Navigation Timing. When the React profiling build is detected (via
//! `console.timeStamp` entries), also reports hydration phases and per-component
//! hydration timing.

use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
pub struct VitalsData {
    pub url: String,
    pub ttfb: Option<f64>,
    pub lcp: Option<Lcp>,
    pub cls: Cls,
    pub fcp: Option<f64>,
    pub inp: Option<f64>,
    pub hydration: Option<HydrationRange>,
    pub phases: Vec<Phase>,
    #[serde(rename = "hydratedComponents")]
    pub hydrated_components: Vec<HydratedComponent>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Lcp {
    #[serde(rename = "startTime")]
    pub start_time: f64,
    pub size: Option<i64>,
    pub element: Option<String>,
    pub url: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Cls {
    pub score: f64,
    pub entries: Vec<ClsEntry>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ClsEntry {
    pub value: f64,
    #[serde(rename = "startTime")]
    pub start_time: f64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct HydrationRange {
    #[serde(rename = "startTime")]
    pub start_time: f64,
    #[serde(rename = "endTime")]
    pub end_time: f64,
    pub duration: f64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Phase {
    pub label: String,
    #[serde(rename = "startTime")]
    pub start_time: f64,
    #[serde(rename = "endTime")]
    pub end_time: f64,
    pub duration: f64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct HydratedComponent {
    pub name: String,
    #[serde(rename = "startTime")]
    pub start_time: f64,
    #[serde(rename = "endTime")]
    pub end_time: f64,
    pub duration: f64,
}

pub fn format_vitals_report(d: &VitalsData) -> String {
    let mut lines: Vec<String> = Vec::new();
    lines.push(format!("# Page Load Profile - {}", d.url));
    lines.push(String::new());
    lines.push("## Core Web Vitals".to_string());

    let ttfb_str = match d.ttfb {
        Some(t) => format!("{}ms", t),
        None => "-".to_string(),
    };
    lines.push(format!("  TTFB {:>10}", ttfb_str));

    match &d.lcp {
        Some(lcp) => {
            let label = match (&lcp.element, &lcp.url) {
                (Some(el), Some(url)) => {
                    let url_trunc: String = url.chars().take(60).collect();
                    format!(" ({}: {})", el, url_trunc)
                }
                (Some(el), None) => format!(" ({})", el),
                _ => String::new(),
            };
            lines.push(format!(
                "  LCP {:>10}{}",
                format!("{}ms", lcp.start_time),
                label
            ));
        }
        None => lines.push("  LCP        -".to_string()),
    }

    lines.push(format!("  CLS {:>10}", d.cls.score));

    if let Some(fcp) = d.fcp {
        lines.push(format!("  FCP {:>10}", format!("{}ms", fcp)));
    }
    if let Some(inp) = d.inp {
        lines.push(format!("  INP {:>10}", format!("{}ms", inp)));
    }

    lines.push(String::new());
    match &d.hydration {
        Some(h) => lines.push(format!(
            "## React Hydration - {}ms ({}ms -> {}ms)",
            h.duration, h.start_time, h.end_time
        )),
        None => {
            lines.push("## React Hydration - no data (requires React profiling build)".to_string())
        }
    }

    if !d.phases.is_empty() {
        for p in &d.phases {
            lines.push(format!(
                "  {:<28} {:>10} ({} -> {})",
                p.label,
                format!("{}ms", p.duration),
                p.start_time,
                p.end_time
            ));
        }
        lines.push(String::new());
    }

    if !d.hydrated_components.is_empty() {
        lines.push(format!(
            "## Hydrated components ({} total, sorted by duration)",
            d.hydrated_components.len()
        ));
        for c in d.hydrated_components.iter().take(30) {
            lines.push(format!(
                "  {:<40} {:>10}",
                c.name,
                format!("{}ms", c.duration)
            ));
        }
        if d.hydrated_components.len() > 30 {
            lines.push(format!(
                "  ... and {} more",
                d.hydrated_components.len() - 30
            ));
        }
    }

    lines.join("\n")
}
