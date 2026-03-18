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

    // Early-exit constants: after we've checked at least this many pixels,
    // if the diff rate exceeds EARLY_EXIT_THRESHOLD, we can stop -- the images
    // are clearly different and the diff image won't be needed.
    const EARLY_EXIT_MIN_PIXELS: u64 = 10_000;
    const EARLY_EXIT_THRESHOLD: f64 = 0.05; // 5% diff rate means clearly not identical

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

            // Early-exit check: once we've seen enough pixels and found enough
            // differences that the final result cannot possibly be "matched" (different == 0),
            // we can skip the remaining pixels and avoid the expensive diff image encoding.
            // We only skip when different > 0 and the diff rate is already above threshold,
            // meaning the result is clearly "not matched" and the diff image is not needed.
            let checked = ((y * wa) + x) as u64 + 1;
            if checked >= EARLY_EXIT_MIN_PIXELS && different > 0 {
                let current_diff_rate = different as f64 / checked as f64;
                if current_diff_rate >= EARLY_EXIT_THRESHOLD {
                    // Early exit -- images are clearly different.
                    // IMPORTANT: `different` here is only the count within the sampled pixels
                    // (up to `checked`), but our output fields are expected to be expressed
                    // relative to the *full* image pixel count.
                    //
                    // So we estimate the overall diff rate using the sample rate and scale it
                    // up to the full image size.
                    let sample_diff_rate = current_diff_rate; // fraction in [0, 1]
                    let mut estimated_different = (sample_diff_rate * total as f64).round();
                    if estimated_different > total as f64 {
                        estimated_different = total as f64;
                    }
                    let mismatch_percentage = (sample_diff_rate * 100.0).min(100.0);

                    return Ok(ScreenshotDiffResult {
                        total_pixels: total,
                        different_pixels: estimated_different as u64,
                        mismatch_percentage,
                        matched: false,
                        diff_image: None,
                        dimension_mismatch: None,
                    });
                }
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

    // -----------------------------------------------------------------------
    // Screenshot diff tests
    // -----------------------------------------------------------------------

    fn make_png(width: u32, height: u32, r: u8, g: u8, b: u8) -> Vec<u8> {
        use image::{ImageBuffer, Rgb, RgbImage};
        let img: RgbImage = ImageBuffer::from_fn(width, height, |_x, _y| Rgb([r, g, b]));
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Png).unwrap();
        buf.into_inner()
    }

    fn make_gradient_png(width: u32, height: u32) -> Vec<u8> {
        use image::{ImageBuffer, Rgb, RgbImage};
        let img: RgbImage = ImageBuffer::from_fn(width, height, |x, y| {
            let r = ((x as f32 / width as f32) * 255.0) as u8;
            let g = ((y as f32 / height as f32) * 255.0) as u8;
            let b = ((x as f32 + y as f32) / ((width + height) as f32) * 255.0) as u8;
            Rgb([r, g, b])
        });
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Png).unwrap();
        buf.into_inner()
    }

    #[test]
    fn test_screenshot_diff_identical() {
        let png = make_png(100, 100, 120, 130, 140);
        let result = diff_screenshot(&png, &png, 0.1).unwrap();
        assert!(result.matched);
        assert_eq!(result.different_pixels, 0);
        assert!((result.mismatch_percentage - 0.0).abs() < 0.01);
        assert!(result.diff_image.is_none()); // No diff image for identical
    }

    #[test]
    fn test_screenshot_diff_totally_different() {
        // Two completely different solid colors -- early exit should fire.
        // Use an image larger than EARLY_EXIT_MIN_PIXELS so the early-exit path
        // has to extrapolate from the sampled pixels to the full image.
        let png_a = make_png(200, 200, 120, 130, 140);
        let png_b = make_png(200, 200, 80, 200, 90);
        let result = diff_screenshot(&png_a, &png_b, 0.1).unwrap();

        assert!(!result.matched);
        assert!(result.diff_image.is_none());

        let total = result.total_pixels;
        assert_eq!(total, 200u64 * 200u64);

        // For totally-different solid colors, the estimated diff rate should be 100%.
        assert_eq!(result.different_pixels, total);
        assert!((result.mismatch_percentage - 100.0).abs() < 0.0001);

        // And the reported mismatch percentage must be consistent with the pixel counts.
        let expected = (result.different_pixels as f64 / result.total_pixels as f64) * 100.0;
        assert!((result.mismatch_percentage - expected).abs() < 0.0001);
    }

    #[test]
    fn test_screenshot_diff_dimension_mismatch() {
        let png_a = make_png(100, 100, 120, 130, 140);
        let png_b = make_png(200, 100, 120, 130, 140);
        let result = diff_screenshot(&png_a, &png_b, 0.1).unwrap();
        assert!(!result.matched);
        assert!(result.dimension_mismatch.is_some());
        assert!(result.diff_image.is_none());
    }

    #[test]
    fn test_screenshot_diff_early_exit_mismatch_scaling() {
        // Construct images where the *first* sampled pixels have ~20% diffs.
        // With the current diff implementation, early-exit will trigger once
        // checked >= 10_000.
        //
        // The key regression fixed in this PR is that the early-exit path must
        // scale the sampled diff rate to the full image pixel count when
        // reporting different_pixels / mismatch_percentage.
        let w = 200u32;
        let h = 200u32;
        let total = (w as u64) * (h as u64);

        let baseline_png = make_png(w, h, 10, 10, 10);

        // Build current image by copying baseline, then changing exactly 20%
        // of pixels within the first 10_000 scan-order pixels.
        let img_a: image::RgbaImage = image::load_from_memory(&baseline_png).unwrap().to_rgba8();
        let mut img_b = img_a.clone();

        for i in 0..10_000u32 {
            if i % 5 != 0 {
                continue; // ~80% same
            }
            let x = i % w;
            let y = i / w;
            // Big color change to guarantee dist > threshold
            img_b.put_pixel(x, y, image::Rgba([200, 200, 200, 255]));
        }

        let mut buf = std::io::Cursor::new(Vec::new());
        img_b.write_to(&mut buf, image::ImageFormat::Png).unwrap();
        let current_png = buf.into_inner();

        let result = diff_screenshot(&baseline_png, &current_png, 0.1).unwrap();

        assert!(!result.matched);
        assert!(result.diff_image.is_none());
        assert_eq!(result.total_pixels, total);

        let expected_diff_rate = 0.20f64;
        let expected_different_pixels = (expected_diff_rate * total as f64).round() as u64;
        let expected_mismatch = expected_diff_rate * 100.0;

        assert_eq!(result.different_pixels, expected_different_pixels);
        assert!((result.mismatch_percentage - expected_mismatch).abs() < 0.0001);

        // And the mismatch must be consistent with the counts.
        let expected_from_counts =
            (result.different_pixels as f64 / result.total_pixels as f64) * 100.0;
        assert!((result.mismatch_percentage - expected_from_counts).abs() < 0.0001);
    }

    #[test]
    fn test_screenshot_diff_gradient_small_change() {
        // Use a large enough image (200x200=40k pixels) with only a small change
        // so the overall diff rate stays well below the 5% early-exit threshold.
        // The diff_image should be generated since early exit won't fire.
        let png_a = make_gradient_png(200, 200);
        // Flip only 1% of pixels spread throughout
        let img_a: image::RgbaImage = image::load_from_memory(&png_a).unwrap().to_rgba8();
        let (w, h) = (img_a.width(), img_a.height());
        let mut img_b = img_a.clone();
        let total = w * h;
        let changed = (total as f32 * 0.01) as u32;
        for i in 0..changed {
            // Deterministic positions spread across the whole image
            let x = (i * 7) % w;
            let y = (i * 13) % h;
            let px = img_a.get_pixel(x, y);
            img_b.put_pixel(
                x,
                y,
                image::Rgba([255 - px[0], 255 - px[1], 255 - px[2], 255]),
            );
        }
        let mut buf = std::io::Cursor::new(Vec::new());
        img_b.write_to(&mut buf, image::ImageFormat::Png).unwrap();
        let png_b = buf.into_inner();

        let result = diff_screenshot(&png_a, &png_b, 0.1).unwrap();
        assert!(!result.matched);
        // diff_image should be present because early-exit threshold not met
        assert!(
            result.diff_image.is_some(),
            "early exit fired unexpectedly: diff_image was None"
        );
        assert!(result.mismatch_percentage > 0.0);
    }
}
