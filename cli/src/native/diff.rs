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

/// Remove ephemeral element refs (`ref=eN`) from snapshot text.
///
/// Refs are reassigned on every snapshot, so two structurally identical pages
/// produce different ref ids and diff as "changed". Stripping refs before
/// diffing keeps `diff url` focused on real content changes. Only the `ref=eN`
/// token (and its attribute-list separator) is removed; other attributes such
/// as `url=` are preserved.
pub fn strip_refs(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    // Scan the unprocessed remainder. Working relative to `rest` (always moving
    // forward) means a separator can never be claimed twice, so no index can
    // invert — strip_refs never panics on arbitrary input.
    let mut rest = text;
    while let Some(pos) = rest.find("ref=e") {
        let digits = rest[pos + 5..]
            .bytes()
            .take_while(|b| b.is_ascii_digit())
            .count();
        let token_end = pos + 5 + digits;
        let before = &rest[..pos];
        let after = &rest[token_end..];

        // A real ref is `eN` (n>0) sitting after ", " or " [". Drop the token
        // with exactly one separator so the `[...]` list stays well-formed;
        // anything else (e.g. "ref=enabled", or text content) is kept verbatim.
        if digits > 0 && before.ends_with(", ") {
            out.push_str(&before[..before.len() - 2]); // "…, ref=eN"
            rest = after;
        } else if digits > 0 && after.starts_with(", ") {
            out.push_str(before); // "ref=eN, …"
            rest = &after[2..];
        } else if digits > 0 && before.ends_with(" [") && after.starts_with(']') {
            out.push_str(&before[..before.len() - 2]); // " [ref=eN]" (only attr)
            rest = &after[1..];
        } else {
            out.push_str(&rest[..token_end]); // not a ref — emit and move on
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_refs_clears_ref_only_attribute() {
        assert_eq!(strip_refs("- link \"Home\" [ref=e4]"), "- link \"Home\"");
    }

    #[test]
    fn test_strip_refs_keeps_preceding_attribute() {
        assert_eq!(
            strip_refs("- heading \"X\" [level=1, ref=e3]"),
            "- heading \"X\" [level=1]"
        );
    }

    #[test]
    fn test_strip_refs_keeps_following_attribute() {
        assert_eq!(
            strip_refs("- link \"Learn\" [ref=e3, url=https://x.com]"),
            "- link \"Learn\" [url=https://x.com]"
        );
    }

    #[test]
    fn test_strip_refs_no_panic_on_adjacent_refs() {
        // Regression: a "delete-range" refactor once panicked here by claiming the
        // same ", " separator twice. strip_refs must tolerate arbitrary text
        // (these aren't real snapshot shapes) without crashing.
        let _ = strip_refs("[ref=e1, ref=e2]");
        let _ = strip_refs("a, ref=e1, ref=e2, b");
        let _ = strip_refs("x [ref=e1] y, ref=e2, z");
    }

    #[test]
    fn test_strip_refs_preserves_ref_like_substring_in_url() {
        // Only real attribute refs are stripped; a "ref=eN" substring inside a
        // url= value must survive untouched.
        assert_eq!(
            strip_refs("- link \"x\" [ref=e3, url=https://x.com/?q=ref=e9]"),
            "- link \"x\" [url=https://x.com/?q=ref=e9]"
        );
        let url_only = "- link \"x\" [url=https://x.com/?q=ref=e9]";
        assert_eq!(strip_refs(url_only), url_only);
    }

    #[test]
    fn test_strip_refs_neutralizes_ref_shift_on_insertion() {
        // An inserted element shifts every later ref number. A line-based diff
        // would then flag every shifted line as changed; stripping refs isolates
        // the single real insertion. (Clearing the ref counter per snapshot only
        // helps identical pages; it does NOT fix this shift case.)
        let a = "- link \"A\" [ref=e1]\n- link \"B\" [ref=e2]\n- link \"C\" [ref=e3]";
        let b = "- link \"X\" [ref=e1]\n- link \"A\" [ref=e2]\n- link \"B\" [ref=e3]\n- link \"C\" [ref=e4]";
        let raw = diff_snapshots(a, b);
        let stripped = diff_snapshots(&strip_refs(a), &strip_refs(b));
        assert!(stripped.additions + stripped.removals < raw.additions + raw.removals);
        assert_eq!(stripped.additions, 1);
        assert_eq!(stripped.removals, 0);
    }

    #[test]
    fn test_strip_refs_makes_ref_only_diff_clean() {
        // Same content, different ref ids -> must not count as a change.
        let a = "- link \"Home\" [ref=e259]\n- link \"About\" [level=1, ref=e260]";
        let b = "- link \"Home\" [ref=e101]\n- link \"About\" [level=1, ref=e102]";
        let result = diff_snapshots(&strip_refs(a), &strip_refs(b));
        assert!(
            !result.changed,
            "ref-only differences should not diff as changed"
        );
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
