//! The saved flow file. Every field is typed; an unknown field is an error.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The file format. Nothing older has shipped.
pub const FLOW_VERSION: u32 = 1;

/// A way of sending a request, in the order a run tries them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Http,
    Page,
    Replay,
}

impl Tier {
    pub const ALL: [Tier; 3] = [Tier::Http, Tier::Page, Tier::Replay];

    pub fn name(self) -> &'static str {
        match self {
            Tier::Http => "http",
            Tier::Page => "page",
            Tier::Replay => "replay",
        }
    }
}

fn yes() -> bool {
    true
}

fn typed_source() -> ParamSource {
    ParamSource::Typed
}

fn is_false(v: &bool) -> bool {
    !*v
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Flow {
    pub version: u32,
    pub name: String,
    pub origin: String,
    pub start_url: String,
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub created_by: Option<CreatedBy>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub redacted_steps: Vec<usize>,
    #[serde(default)]
    pub params: Vec<Param>,
    #[serde(default)]
    pub output: String,
    #[serde(default)]
    pub item_fields: Vec<String>,
    #[serde(default)]
    pub provenance: String,
    #[serde(default)]
    pub verified_at: Option<String>,
    #[serde(default)]
    pub writes: Vec<Write>,
    pub steps: Vec<Step>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreatedBy {
    pub tool: String,
    pub version: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ParamSource {
    Typed,
    Query,
    Response,
}

/// Where a param's value comes from when it is not typed.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ParamFrom {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_query: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Param {
    pub name: String,
    /// The recorded value. Typed params keep none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub example: Option<String>,
    #[serde(default = "typed_source")]
    pub source: ParamSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<ParamFrom>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WriteKind {
    Explicit,
    Suspected,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Write {
    pub id: String,
    pub method: String,
    #[serde(default)]
    pub url: String,
    pub kind: WriteKind,
    #[serde(default)]
    pub step: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    #[serde(default)]
    pub seq: u64,
    #[serde(default = "yes")]
    pub ok: bool,
    /// The recorded command, as the CLI sent it.
    #[serde(default)]
    pub command: Value,
    #[serde(default)]
    pub requests: Vec<Request>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub id: String,
    pub method: String,
    pub url: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub body: Option<String>,
    /// The tier that worked last time; absent means start with HTTP.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<Tier>,
    /// A run needs this request allowed before it sends it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub write: bool,
    #[serde(default)]
    pub expect: Expect,
}

/// What the recorded response looked like.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Expect {
    #[serde(default)]
    pub status: Option<u16>,
    #[serde(default)]
    pub mime: String,
    #[serde(default)]
    pub signature: Value,
    #[serde(default)]
    pub bytes: u64,
    #[serde(default)]
    pub item_fields: Vec<String>,
    #[serde(default)]
    pub extract: Option<Value>,
    /// The recording sent credential headers that a run cannot copy.
    #[serde(default)]
    pub stripped_auth: bool,
}

impl Flow {
    pub fn requests(&self) -> impl Iterator<Item = &Request> {
        self.steps.iter().flat_map(|s| s.requests.iter())
    }

    pub fn request(&self, id: &str) -> Option<&Request> {
        self.requests().find(|r| r.id == id)
    }

    pub fn write(&self, id: &str) -> Option<&Write> {
        self.writes.iter().find(|w| w.id == id)
    }
}
