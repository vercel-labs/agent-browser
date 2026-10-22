use async_trait::async_trait;
use serde_json::Value;

use crate::native::cdp::client::{CdpClient, RawCdpMessage};

#[derive(Clone, Debug)]
pub(super) enum SourceSignal {
    Match {
        key: String,
        name: String,
        data: Value,
        initial: bool,
    },
    Rearmed,
    Rejected,
}

#[async_trait]
#[allow(clippy::double_must_use)]
pub(super) trait EventSource: Send + Sync {
    fn supported_engines(&self) -> &'static [&'static str];
    fn policy_actions(&self) -> &[&'static str];
    fn target_id(&self) -> &str;
    fn session_id(&self) -> &str;
    fn sharing_key(&self) -> Option<String>;
    fn classify(&self, raw: &RawCdpMessage) -> Option<SourceSignal>;
    async fn observe(&self, client: &CdpClient) -> Result<Option<SourceSignal>, String>;
    async fn stop(&self, client: &CdpClient);
}
