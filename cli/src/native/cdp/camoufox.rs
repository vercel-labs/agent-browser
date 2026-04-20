//! Camoufox sidecar process lifecycle (stubbed in Unit 1).
//!
//! Unit 1 only defines the type so `BrowserProcess::Camoufox(CamoufoxProcess)`
//! compiles. `BrowserManager::launch` returns a structured
//! `not-yet-implemented` error before anything tries to construct this value.
//! Unit 3 fills in the real Python-sidecar child-process lifecycle, mirroring
//! `LightpandaProcess`.

use std::time::Duration;

/// Placeholder for the Python sidecar subprocess. In Unit 3 this gains
/// ownership of the `std::process::Child`, the stdio handles, and bounded
/// log-drainer threads (mirrors `LightpandaProcess`).
pub struct CamoufoxProcess {
    _private: (),
}

impl CamoufoxProcess {
    pub fn kill(&mut self) {
        // No-op: Unit 1 cannot construct a live sidecar. Unit 3 replaces this
        // with `child.kill()` + drainer teardown.
    }

    pub fn wait_or_kill(&mut self, _timeout: Duration) {
        // No-op for the same reason as `kill`.
    }
}

impl Drop for CamoufoxProcess {
    fn drop(&mut self) {
        self.kill();
    }
}
