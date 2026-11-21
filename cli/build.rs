use std::fs;
use std::path::Path;

const DASHBOARD_PLACEHOLDER: &str = "<!DOCTYPE html><html><body><p>Dashboard not built. Run: cd packages/dashboard &amp;&amp; pnpm build</p></body></html>\n";

/// Ensure `packages/dashboard/out/` exists so `rust-embed` doesn't fail during
/// Rust-only dev builds where the dashboard hasn't been built. The placeholder
/// `index.html` is only written when the directory is completely absent, and
/// every build that embeds it warns.
fn ensure_dashboard_dir() {
    let dashboard_out = Path::new("../packages/dashboard/out");
    let index = dashboard_out.join("index.html");
    println!("cargo:rerun-if-changed=../packages/dashboard/out");
    if !index.exists() {
        let _ = fs::create_dir_all(dashboard_out);
        let _ = fs::write(&index, DASHBOARD_PLACEHOLDER);
    }
    if fs::read_to_string(&index).is_ok_and(|html| html == DASHBOARD_PLACEHOLDER) {
        println!(
            "cargo:warning=packages/dashboard/out is not built, so the binary embeds a placeholder instead of the dashboard. Build from source with `pnpm run build:source`."
        );
    }
}

fn main() {
    ensure_dashboard_dir();
}
