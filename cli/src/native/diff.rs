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
    let max_color_distance_squared = threshold * threshold * 255.0 * 255.0 * 3.0;
    let mut different = 0u64;

    let mut diff_img = image::RgbaImage::new(wa, ha);

    for y in 0..ha {
        for x in 0..wa {
            let pa = rgba_a.get_pixel(x, y);
            let pb = rgba_b.get_pixel(x, y);
            // Compare the visible colors on a white background. RGB values
            // beneath transparent pixels do not describe visible changes.
            let visible = |pixel: &image::Rgba<u8>, channel: usize| {
                let alpha = pixel[3] as u32;
                (pixel[channel] as u32 * alpha + 255 * (255 - alpha)) as f64 / 255.0
            };
            let dr = visible(pa, 0) - visible(pb, 0);
            let dg = visible(pa, 1) - visible(pb, 1);
            let db = visible(pa, 2) - visible(pb, 2);
            let distance_squared = dr * dr + dg * dg + db * db;

            if distance_squared > max_color_distance_squared {
                different += 1;
                diff_img.put_pixel(x, y, image::Rgba([255, 0, 0, 255]));
            } else {
                let gray = ((visible(pa, 0) + visible(pa, 1) + visible(pa, 2)) / 3.0) as u8;
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
    // Fast path: identical inputs.
    // This avoids constructing the `similar` TextDiff object and running the diff
    // iteration when agents compare a snapshot to itself (common in retry/loop
    // workloads).
    if before == after {
        let unchanged = before.lines().count();
        return SnapshotDiffResult {
            diff: String::new(),
            additions: 0,
            removals: 0,
            unchanged,
            changed: false,
        };
    }

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
    fn screenshot_diff_detects_a_visible_alpha_change() {
        let result =
            diff_screenshot(&pixel_png([0, 0, 0, 0]), &pixel_png([0, 0, 0, 255]), 0.0).unwrap();
        assert!(!result.matched);
        assert_eq!(result.different_pixels, 1);
        assert_eq!(result.mismatch_percentage, 100.0);
    }

    #[test]
    fn screenshot_diff_ignores_colors_beneath_full_transparency() {
        let result =
            diff_screenshot(&pixel_png([255, 0, 0, 0]), &pixel_png([0, 0, 255, 0]), 0.0).unwrap();
        assert!(result.matched);
        assert!(result.diff_image.is_none());
    }

    #[test]
    fn screenshot_diff_compares_composited_partial_transparency() {
        let result = diff_screenshot(
            &pixel_png([0, 0, 0, 128]),
            &pixel_png([127, 127, 127, 255]),
            0.0,
        )
        .unwrap();
        assert!(result.matched);
    }

    #[test]
    fn screenshot_diff_preserves_opaque_thresholds() {
        let black = pixel_png([0, 0, 0, 255]);
        let white = pixel_png([255, 255, 255, 255]);
        assert!(diff_screenshot(&black, &white, 1.0).unwrap().matched);
        assert!(!diff_screenshot(&black, &white, 0.99).unwrap().matched);
    }

    fn pixel_png(color: [u8; 4]) -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(1, 1, image::Rgba(color));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        bytes.into_inner()
    }

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

    #[test]
    fn test_diff_snapshots_identical_fast_path() {
        let input = "hello\nworld\n";
        let result = diff_snapshots(input, input);
        assert!(!result.changed);
        assert_eq!(result.additions, 0);
        assert_eq!(result.removals, 0);
        assert_eq!(result.unchanged, input.lines().count());
        assert!(result.diff.is_empty());
    }

    #[test]
    #[ignore]
    fn bench_diff_snapshots_identical_and_changed() {
        use std::hint::black_box;
        use std::time::Instant;

        let identical_a = (0..200)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let identical_b = identical_a.clone();

        let changed_a = identical_a.clone();
        let changed_b = (0..200)
            .map(|i| {
                if i == 123 {
                    format!("line {i} changed")
                } else {
                    format!("line {i}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n");

        // Keep the iteration count high enough to measure, but low enough
        // to avoid long CI times when someone runs `--ignored`.
        let iters = 50_000usize;

        let start = Instant::now();
        let mut acc_changed = 0usize;
        for _ in 0..iters {
            let r = diff_snapshots(black_box(&identical_a), black_box(&identical_b));
            acc_changed ^= r.unchanged;
        }
        let identical_ms = start.elapsed().as_secs_f64() * 1000.0;

        let start = Instant::now();
        let mut acc_changed2 = 0usize;
        for _ in 0..iters {
            let r = diff_snapshots(black_box(&changed_a), black_box(&changed_b));
            acc_changed2 ^= r.additions;
        }
        let changed_ms = start.elapsed().as_secs_f64() * 1000.0;

        // Prevent the compiler from optimizing everything away.
        black_box(acc_changed);
        black_box(acc_changed2);

        println!(
            "bench_diff_snapshots_identical_and_changed: iters={iters} identical_ms={identical_ms:.2} changed_ms={changed_ms:.2}"
        );
    }
}

/// Remove generated ref annotations for content comparisons across documents.
/// Quoted names are preserved even when they contain text resembling a ref.
pub fn snapshot_comparison_text(tree: &str) -> String {
    let mut output = String::with_capacity(tree.len());
    let mut chars = tree.chars().peekable();
    let mut quoted = false;
    while let Some(ch) = chars.next() {
        if quoted && ch == '\\' {
            output.push(ch);
            if let Some(escaped) = chars.next() {
                output.push(escaped);
            }
            continue;
        }
        if ch == '"' {
            quoted = !quoted;
        }
        if !quoted && ch == '[' && output.ends_with(' ') {
            let mut candidate = chars.clone();
            let mut attributes = String::new();
            while candidate.peek().is_some_and(|ch| !matches!(ch, ']' | '\n')) {
                attributes.push(candidate.next().unwrap());
            }
            if candidate.next() == Some(']') {
                let fields: Vec<&str> = attributes.split(", ").collect();
                let retained: Vec<&str> = fields
                    .iter()
                    .copied()
                    .filter(|field| {
                        !field.strip_prefix("ref=e").is_some_and(|id| {
                            !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit())
                        })
                    })
                    .collect();
                if retained.len() != fields.len() {
                    if retained.is_empty() {
                        output.pop();
                    } else {
                        output.push('[');
                        output.push_str(&retained.join(", "));
                        output.push(']');
                    }
                    chars = candidate;
                    continue;
                }
            }
        }
        output.push(ch);
    }
    output
}

#[cfg(test)]
mod durable_ref_diff_tests {
    use super::*;

    #[test]
    fn content_comparison_ignores_only_generated_ref_annotations() {
        let before =
            "- button \"Save [ref=e3]\" [ref=e1]\n- checkbox \"Agree\" [checked=false, ref=e2]";
        let after =
            "- button \"Save [ref=e3]\" [ref=e20]\n- checkbox \"Agree\" [checked=false, ref=e21]";
        let normalized = snapshot_comparison_text(before);
        assert!(normalized.contains("Save [ref=e3]"));
        assert!(normalized.contains("[checked=false]"));
        assert!(!diff_snapshots(&normalized, &snapshot_comparison_text(after)).changed);
        assert!(
            diff_snapshots(
                &normalized,
                &snapshot_comparison_text(&after.replace("Agree", "Agreed"))
            )
            .changed
        );
    }
}
