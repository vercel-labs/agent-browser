//! Placeholder client for the Camoufox backend.
//!
//! Unit 1 defines the type so that `BrowserBackend::Camoufox(Arc<CamoufoxClient>)`
//! compiles and action-layer dispatch can grow a `Camoufox` arm. The real
//! sidecar-driven implementation (reader/writer tasks, JSON-line protocol,
//! request/response demux) lands in Unit 3.

/// Marker client for engine=camoufox. No state in Unit 1 — Unit 3 fills in
/// stdio handles to the Python sidecar, a pending-request map, and broadcast
/// channels for asynchronous `{"event": ...}` frames.
pub struct CamoufoxClient {
    _private: (),
}

impl CamoufoxClient {
    /// Construct a stub client. This is the only way to produce a
    /// `CamoufoxClient` in Unit 1; action-layer code that matches on
    /// `BrowserBackend::Camoufox` surfaces a structured
    /// `not-yet-implemented` error rather than touching this value.
    pub fn stub() -> Self {
        Self { _private: () }
    }
}

impl std::fmt::Debug for CamoufoxClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CamoufoxClient").finish_non_exhaustive()
    }
}
