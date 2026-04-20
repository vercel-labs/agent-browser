//! Embedded Camoufox sidecar package.
//!
//! The full `camoufox_sidecar` Python package (multi-file, sibling imports)
//! is baked into the agent-browser binary via `include_dir!` so users who
//! install only the Rust binary still get a working sidecar to spawn. On
//! first launch we extract the tree into a version-keyed cache directory and
//! spawn `python3 <dir>/__main__.py` with `PYTHONPATH` pointed at the
//! extraction dir so sibling imports resolve.
//!
//! The extraction dir is keyed by the crate version so upgrades re-extract
//! deterministically. A `.extracted` sentinel marks a completed extraction;
//! subsequent launches observing the sentinel skip re-extraction so process
//! startup stays fast and the files' mtimes are stable.
//!
//! In E2B (and other environments where the sidecar is `pip install`'d) we
//! prefer `python3 -m camoufox_sidecar` and only fall back to the extracted
//! tree if the module import fails — handled in `camoufox_client.rs`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use include_dir::{include_dir, Dir};

/// Embedded Python package. Path is resolved at compile time by `include_dir!`
/// against `$CARGO_MANIFEST_DIR` (the `cli/` crate root).
static SIDECAR_PACKAGE: Dir<'_> =
    include_dir!("$CARGO_MANIFEST_DIR/../packages/camoufox-sidecar/camoufox_sidecar");

/// Filename written inside the extracted tree once extraction has completed
/// successfully. Its presence is the signal that the tree is safe to use.
const EXTRACTED_SENTINEL: &str = ".extracted";

/// Root of the version-keyed extraction tree for this crate build. The
/// sidecar package itself lives in a `camoufox_sidecar/` subdirectory of
/// this root; callers point `PYTHONPATH` at the root and spawn
/// `python3 -m camoufox_sidecar`.
pub fn extraction_root() -> io::Result<PathBuf> {
    let base = dirs::cache_dir().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "no user cache directory available (dirs::cache_dir returned None)",
        )
    })?;
    Ok(base.join(format!(
        "agent-browser/camoufox-sidecar-{}",
        env!("CARGO_PKG_VERSION")
    )))
}

/// Path to the extracted `camoufox_sidecar` Python package directory. This
/// is `extraction_root()/camoufox_sidecar/` — the name must stay in sync
/// with the Python import name (hence the underscore rather than the dash
/// the outer directory uses for the crate version).
pub fn package_dir() -> io::Result<PathBuf> {
    Ok(extraction_root()?.join("camoufox_sidecar"))
}

/// Ensure the embedded sidecar package is laid out on disk and return the
/// PYTHONPATH root (the directory that contains the `camoufox_sidecar`
/// package). If the sentinel file is already present we skip extraction so
/// mtimes stay stable (see the "running twice in a row" test scenario in
/// the Camoufox engine plan).
///
/// Extraction is best-effort atomic: we extract into a staging directory
/// and rename into place, so a crash mid-extraction cannot leave a
/// half-populated tree that is then reused on the next launch.
pub fn ensure_extracted() -> io::Result<PathBuf> {
    let root = extraction_root()?;
    if is_already_extracted(&root) {
        return Ok(root);
    }

    if let Some(parent) = root.parent() {
        fs::create_dir_all(parent)?;
    }

    let staging = staging_dir_for(&root);
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir_all(&staging)?;
    let package_in_staging = staging.join("camoufox_sidecar");
    fs::create_dir_all(&package_in_staging)?;

    SIDECAR_PACKAGE.extract(&package_in_staging)?;
    fs::write(
        staging.join(EXTRACTED_SENTINEL),
        env!("CARGO_PKG_VERSION").as_bytes(),
    )?;

    if root.exists() {
        let _ = fs::remove_dir_all(&root);
    }
    fs::rename(&staging, &root)?;

    Ok(root)
}

/// True if `path` already hosts a successfully-extracted package. We check
/// the sentinel specifically because `camoufox_sidecar/__main__.py` alone
/// could be the remnant of an interrupted extraction.
fn is_already_extracted(path: &Path) -> bool {
    path.join(EXTRACTED_SENTINEL).is_file()
        && path.join("camoufox_sidecar").join("__main__.py").is_file()
}

fn staging_dir_for(target: &Path) -> PathBuf {
    let mut staging = target.as_os_str().to_owned();
    staging.push(format!(".staging-{}", std::process::id()));
    PathBuf::from(staging)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // `std::env::set_var` is process-global. Serialise the tests that touch
    // $XDG_CACHE_HOME so parallel cargo test runs don't race.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_temp_cache<F: FnOnce(&Path)>(f: F) {
        let guard = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let prev_xdg = std::env::var_os("XDG_CACHE_HOME");
        let prev_home = std::env::var_os("HOME");
        std::env::set_var("XDG_CACHE_HOME", tmp.path());
        std::env::set_var("HOME", tmp.path());
        f(tmp.path());
        if let Some(v) = prev_xdg {
            std::env::set_var("XDG_CACHE_HOME", v);
        } else {
            std::env::remove_var("XDG_CACHE_HOME");
        }
        if let Some(v) = prev_home {
            std::env::set_var("HOME", v);
        } else {
            std::env::remove_var("HOME");
        }
        drop(guard);
    }

    #[test]
    fn extracts_all_expected_files() {
        with_temp_cache(|_| {
            let root = ensure_extracted().unwrap();
            let pkg = root.join("camoufox_sidecar");
            assert!(pkg.join("__main__.py").is_file());
            assert!(pkg.join("protocol.py").is_file());
            assert!(pkg.join("session.py").is_file());
            assert!(root.join(EXTRACTED_SENTINEL).is_file());
        });
    }

    #[test]
    fn second_call_is_idempotent_and_preserves_mtime() {
        with_temp_cache(|_| {
            let dir = ensure_extracted().unwrap();
            let marker = dir.join(EXTRACTED_SENTINEL);
            let first = fs::metadata(&marker).unwrap().modified().unwrap();

            // Small sleep so a re-extract would show up as a newer mtime on
            // filesystems with second-level precision.
            std::thread::sleep(std::time::Duration::from_millis(1100));

            let dir2 = ensure_extracted().unwrap();
            assert_eq!(dir, dir2);
            let second = fs::metadata(&marker).unwrap().modified().unwrap();
            assert_eq!(first, second, "sentinel mtime should be unchanged");
        });
    }
}
