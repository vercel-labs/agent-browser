//! Firefox screenshots and PDF.

use base64::Engine;
use serde_json::{json, Value};

use super::actions::session;
use crate::native::actions::DaemonState;
use crate::native::screenshot::{save_screenshot, ScreenshotOptions};

const NOT_YET: &str = "not yet supported on Firefox";

fn screenshot_format(cmd: &Value) -> String {
    cmd.get("format")
        .or_else(|| cmd.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("png")
        .to_string()
}

fn screenshot_options(cmd: &Value) -> ScreenshotOptions {
    ScreenshotOptions {
        selector: None,
        path: cmd.get("path").and_then(Value::as_str).map(String::from),
        full_page: cmd
            .get("fullPage")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        format: screenshot_format(cmd),
        quality: cmd.get("quality").and_then(Value::as_i64).map(|q| q as i32),
        annotate: false,
        output_dir: cmd
            .get("screenshotDir")
            .and_then(Value::as_str)
            .map(String::from),
    }
}

fn reject_unsupported_screenshot_options(cmd: &Value) -> Result<(), String> {
    if let Some(selector) = cmd.get("selector").and_then(Value::as_str) {
        return Err(format!(
            "Element screenshots are {NOT_YET} (selector `{selector}`)"
        ));
    }
    if cmd
        .get("annotate")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err(format!("Annotated screenshots are {NOT_YET}"));
    }
    if cmd
        .get("ifChanged")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err(format!(
            "Conditional screenshots (`ifChanged`) are {NOT_YET}"
        ));
    }
    Ok(())
}

/// The BiDi parameters for a viewport or full-page screenshot of `context`.
fn screenshot_params(cmd: &Value, context: &str) -> Result<Value, String> {
    reject_unsupported_screenshot_options(cmd)?;
    let format = screenshot_format(cmd);
    let mime = match format.as_str() {
        "png" => "image/png",
        "jpeg" => "image/jpeg",
        other => return Err(format!("Unsupported screenshot format on Firefox: {other}")),
    };
    let full_page = cmd
        .get("fullPage")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut params = json!({
        "context": context,
        "origin": if full_page { "document" } else { "viewport" },
        "format": { "type": mime },
    });
    if format == "jpeg" {
        let quality = cmd.get("quality").and_then(Value::as_i64).unwrap_or(80);
        params["format"]["quality"] = json!(quality as f64 / 100.0);
    }
    Ok(params)
}

/// The BiDi parameters for a PDF of `context`, following Chrome's `handle_pdf` options.
fn pdf_params(cmd: &Value, context: &str) -> Value {
    let landscape = cmd
        .get("landscape")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let prefer_css_page_size = cmd
        .get("preferCSSPageSize")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    json!({
        "context": context,
        "background": cmd.get("printBackground").and_then(Value::as_bool).unwrap_or(true),
        "orientation": if landscape { "landscape" } else { "portrait" },
        "shrinkToFit": !prefer_css_page_size,
    })
}

fn pdf_save_path(cmd: &Value) -> String {
    match cmd.get("path").and_then(Value::as_str) {
        Some(path) => path.to_string(),
        None => {
            let dir = dirs::home_dir()
                .unwrap_or_else(std::env::temp_dir)
                .join(".agent-browser")
                .join("tmp")
                .join("pdfs");
            let _ = std::fs::create_dir_all(&dir);
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            dir.join(format!("page-{}.pdf", timestamp))
                .to_string_lossy()
                .to_string()
        }
    }
}

/// Viewport or full-page screenshot (`browsingContext.captureScreenshot`, `origin: document` for
/// full page). Must return what Chrome's `handle_screenshot` returns for the same flags.
pub async fn screenshot(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let options = screenshot_options(cmd);
    let ff = session(state)?;
    let params = screenshot_params(cmd, &ff.context)?;
    let result = ff
        .bidi
        .send("browsingContext.captureScreenshot", params)
        .await?;
    let data = result
        .get("data")
        .and_then(Value::as_str)
        .ok_or("No screenshot data returned")?;
    Ok(json!({ "path": save_screenshot(data, &options)? }))
}

/// PDF of the active tab (`browsingContext.print`). Must return what Chrome's `handle_pdf` returns.
pub async fn pdf(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let ff = session(state)?;
    let params = pdf_params(cmd, &ff.context);
    let result = ff.bidi.send("browsingContext.print", params).await?;
    let data = result
        .get("data")
        .and_then(Value::as_str)
        .ok_or("No PDF data returned")?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|e| format!("Failed to decode PDF: {}", e))?;
    let save_path = pdf_save_path(cmd);
    std::fs::write(&save_path, &bytes).map_err(|e| format!("Failed to save PDF: {}", e))?;
    Ok(json!({ "path": save_path }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn viewport_png_maps_to_bidi_screenshot() {
        let params = screenshot_params(&json!({}), "ctx-1").unwrap();
        assert_eq!(
            params,
            json!({
                "context": "ctx-1",
                "origin": "viewport",
                "format": {"type": "image/png"},
            })
        );
    }

    #[test]
    fn full_page_jpeg_carries_quality_as_a_fraction() {
        let params = screenshot_params(
            &json!({"fullPage": true, "format": "jpeg", "quality": 55}),
            "c",
        )
        .unwrap();
        assert_eq!(params["origin"], "document");
        assert_eq!(
            params["format"],
            json!({"type": "image/jpeg", "quality": 0.55})
        );
        let default = screenshot_params(&json!({"format": "jpeg"}), "c").unwrap();
        assert_eq!(default["format"]["quality"], 0.8);
    }

    #[test]
    fn type_aliases_format_and_png_ignores_quality() {
        let params = screenshot_params(&json!({"type": "jpeg", "quality": 30}), "c").unwrap();
        assert_eq!(
            params["format"],
            json!({"type": "image/jpeg", "quality": 0.3})
        );
        let png = screenshot_params(&json!({"format": "png", "quality": 30}), "c").unwrap();
        assert_eq!(png["format"], json!({"type": "image/png"}));
        let e = screenshot_params(&json!({"format": "webp"}), "c").unwrap_err();
        assert!(e.contains("Unsupported screenshot format"), "{e}");
    }

    #[test]
    fn element_annotated_and_conditional_screenshots_are_rejected() {
        let element = screenshot_params(&json!({"selector": "#main"}), "c").unwrap_err();
        assert!(
            element.contains("Element screenshots are not yet supported on Firefox"),
            "{element}"
        );
        let annotated = screenshot_params(&json!({"annotate": true}), "c").unwrap_err();
        assert!(
            annotated.contains("Annotated screenshots are not yet supported on Firefox"),
            "{annotated}"
        );
        let conditional = screenshot_params(&json!({"ifChanged": true}), "c").unwrap_err();
        assert!(
            conditional.contains("not yet supported on Firefox"),
            "{conditional}"
        );
    }

    #[test]
    fn pdf_maps_chrome_options_to_bidi_print() {
        let defaults = pdf_params(&json!({}), "ctx-1");
        assert_eq!(
            defaults,
            json!({
                "context": "ctx-1",
                "background": true,
                "orientation": "portrait",
                "shrinkToFit": true,
            })
        );
        let custom = pdf_params(
            &json!({"printBackground": false, "landscape": true, "preferCSSPageSize": true}),
            "ctx-2",
        );
        assert_eq!(custom["background"], false);
        assert_eq!(custom["orientation"], "landscape");
        assert_eq!(custom["shrinkToFit"], false);
    }
}
