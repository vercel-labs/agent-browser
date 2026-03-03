use serde_json::{json, Value};
use similar::{ChangeTag, TextDiff};

pub struct ScreenshotDiffResult {
    pub total_pixels: u64,
    pub different_pixels: u64,
    pub mismatch_percentage: f64,
    pub matched: bool,
    pub diff_image: Option<Vec<u8>>,
    pub dimension_mismatch: Option<Value>,
}

pub struct SnapshotDiffResult {
    pub diff: String,
    pub additions: usize,
    pub removals: usize,
    pub unchanged: usize,
    pub changed: bool,
}

pub fn diff_screenshot(
    baseline: &[u8],
    current: &[u8],
    threshold: f64,
) -> Result<ScreenshotDiffResult, String> {
    let img_a = image::load_from_memory(baseline)
        .map_err(|e| format!("Failed to decode baseline image: {}", e))?;
    let img_b = image::load_from_memory(current)
        .map_err(|e| format!("Failed to decode current image: {}", e))?;

    let (wa, ha) = (img_a.width(), img_a.height());
    let (wb, hb) = (img_b.width(), img_b.height());

    if wa != wb || ha != hb {
        return Ok(ScreenshotDiffResult {
            total_pixels: (wa as u64) * (ha as u64),
            different_pixels: (wa as u64) * (ha as u64),
            mismatch_percentage: 100.0,
            matched: false,
            diff_image: None,
            dimension_mismatch: Some(json!({
                "expected": { "width": wa, "height": ha },
                "actual": { "width": wb, "height": hb },
            })),
        });
    }

    let rgba_a = img_a.to_rgba8();
    let rgba_b = img_b.to_rgba8();
    let total = (wa as u64) * (ha as u64);
    let max_color_distance = threshold * 255.0 * (3.0_f64).sqrt();
    let mut different = 0u64;

    let mut diff_img = image::RgbaImage::new(wa, ha);

    for y in 0..ha {
        for x in 0..wa {
            let pa = rgba_a.get_pixel(x, y);
            let pb = rgba_b.get_pixel(x, y);
            let dr = (pa[0] as f64) - (pb[0] as f64);
            let dg = (pa[1] as f64) - (pb[1] as f64);
            let db = (pa[2] as f64) - (pb[2] as f64);
            let dist = (dr * dr + dg * dg + db * db).sqrt();

            if dist > max_color_distance {
                different += 1;
                diff_img.put_pixel(x, y, image::Rgba([255, 0, 0, 255]));
            } else {
                let gray = ((pa[0] as u16 + pa[1] as u16 + pa[2] as u16) / 3) as u8;
                let dimmed = (gray as f64 * 0.3) as u8;
                diff_img.put_pixel(x, y, image::Rgba([dimmed, dimmed, dimmed, 255]));
            }
        }
    }

    let mismatch = if total > 0 {
        (different as f64 / total as f64) * 100.0
    } else {
        0.0
    };

    let diff_bytes = if different > 0 {
        let mut buf = std::io::Cursor::new(Vec::new());
        diff_img
            .write_to(&mut buf, image::ImageFormat::Png)
            .map_err(|e| format!("Failed to encode diff image: {}", e))?;
        Some(buf.into_inner())
    } else {
        None
    };

    Ok(ScreenshotDiffResult {
        total_pixels: total,
        different_pixels: different,
        mismatch_percentage: mismatch,
        matched: different == 0,
        diff_image: diff_bytes,
        dimension_mismatch: None,
    })
}

/// Compute a snapshot diff using the Myers algorithm via the `similar` crate.
pub fn diff_snapshots(before: &str, after: &str) -> SnapshotDiffResult {
    let text_diff = TextDiff::from_lines(before, after);

    let mut additions = 0usize;
    let mut removals = 0usize;
    let mut unchanged = 0usize;

    for change in text_diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Insert => additions += 1,
            ChangeTag::Delete => removals += 1,
            ChangeTag::Equal => unchanged += 1,
        }
    }

    let changed = additions > 0 || removals > 0;

    let diff = text_diff
        .unified_diff()
        .context_radius(3)
        .header("before", "after")
        .to_string();

    SnapshotDiffResult {
        diff,
        additions,
        removals,
        unchanged,
        changed,
    }
}

/// Legacy JSON diff output for backwards compatibility.
pub fn diff_text(a: &str, b: &str) -> Value {
    let result = diff_snapshots(a, b);
    json!({
        "identical": !result.changed,
        "additions": result.additions,
        "removals": result.removals,
        "deletions": result.removals,
        "unchanged": result.unchanged,
        "changed": result.changed,
    })
}

pub fn diff_unified(a: &str, b: &str) -> String {
    diff_snapshots(a, b).diff
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_diff_identical() {
        let result = diff_text("hello\nworld", "hello\nworld");
        assert_eq!(result.get("identical").unwrap(), true);
        assert_eq!(result.get("changed").unwrap(), false);
        assert_eq!(result.get("unchanged").unwrap(), 2);
    }

    #[test]
    fn test_diff_additions() {
        let result = diff_text("hello\n", "hello\nworld\n");
        assert_eq!(result.get("identical").unwrap(), false);
        assert_eq!(result.get("changed").unwrap(), true);
        assert!(result.get("additions").unwrap().as_i64().unwrap() > 0);
    }

    #[test]
    fn test_diff_deletions() {
        let result = diff_text("hello\nworld\n", "hello\n");
        assert_eq!(result.get("identical").unwrap(), false);
        assert!(result.get("removals").unwrap().as_i64().unwrap() > 0);
    }

    #[test]
    fn test_diff_unified_output() {
        let output = diff_unified("a\nb\nc\n", "a\nx\nc\n");
        assert!(output.contains("---"));
        assert!(output.contains("+++"));
    }

    #[test]
    fn test_snapshot_diff_struct() {
        let result = diff_snapshots("line1\nline2\n", "line1\nline3\n");
        assert!(result.changed);
        assert_eq!(result.additions, 1);
        assert_eq!(result.removals, 1);
        assert_eq!(result.unchanged, 1);
        assert!(!result.diff.is_empty());
    }
}
