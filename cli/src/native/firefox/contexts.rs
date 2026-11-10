//! The browsing contexts Firefox reports: tabs, popups and frames, the tab each belongs to, the
//! cookie jar each uses and the one commands act on.

use std::collections::{HashMap, HashSet};

use serde_json::{json, Value};

const DEFAULT_USER_CONTEXT: &str = "default";

/// A top-level browsing context.
#[derive(Debug, Clone, PartialEq)]
pub struct Tab {
    /// Stable `t<N>` number, assigned on first sight and never reused.
    pub id: u32,
    pub label: Option<String>,
    /// BiDi user context, the cookie jar Firefox reports for this tab.
    pub user_context: String,
}

#[derive(Debug)]
enum Node {
    Tab(Tab),
    Frame { parent: String },
}

/// Contexts that left the tree.
#[derive(Debug, Default)]
pub struct Removed {
    /// Every context that is gone, nested ones included.
    pub contexts: HashSet<String>,
    /// True when the active context was among them, so another one took over.
    pub active_changed: bool,
}

/// Every browsing context of one Firefox session. All questions about tab ownership, cookie
/// jars and the active context are answered here and nowhere else.
#[derive(Debug)]
pub struct Contexts {
    nodes: HashMap<String, Node>,
    active: String,
    next_tab: u32,
    owned_user_contexts: HashSet<String>,
}

impl Contexts {
    /// Builds the registry from a full `browsingContext.getTree` reply with its first tab active.
    pub fn from_tree(tree: &Value) -> Result<Self, String> {
        let active = tree["contexts"][0]["context"]
            .as_str()
            .ok_or("Firefox reported no browsing context")?
            .to_string();
        let mut contexts = Self {
            nodes: HashMap::new(),
            active,
            next_tab: 1,
            owned_user_contexts: HashSet::new(),
        };
        contexts.sync(tree);
        Ok(contexts)
    }

    /// Records one `browsingContext.Info`, a `getTree` entry or a `contextCreated` event, along
    /// with the contexts nested in it. Returns true when it became the active context because
    /// no open tab was active.
    pub fn record(&mut self, info: &Value) -> bool {
        self.record_under(info, None);
        let context = info["context"].as_str().unwrap_or_default();
        let adopted = !self.nodes.contains_key(&self.active)
            && matches!(self.nodes.get(context), Some(Node::Tab(_)));
        if adopted {
            self.active = context.to_string();
        }
        adopted
    }

    fn record_under(&mut self, info: &Value, parent: Option<&str>) {
        let Some(context) = info["context"].as_str() else {
            return;
        };
        match info["parent"].as_str().or(parent) {
            Some(parent) => {
                self.nodes.insert(
                    context.to_string(),
                    Node::Frame {
                        parent: parent.to_string(),
                    },
                );
            }
            None => {
                let user_context = info["userContext"]
                    .as_str()
                    .unwrap_or(DEFAULT_USER_CONTEXT)
                    .to_string();
                if let Some(Node::Tab(tab)) = self.nodes.get_mut(context) {
                    tab.user_context = user_context;
                } else {
                    let id = self.next_tab;
                    self.next_tab += 1;
                    self.nodes.insert(
                        context.to_string(),
                        Node::Tab(Tab {
                            id,
                            label: None,
                            user_context,
                        }),
                    );
                }
            }
        }
        for child in info["children"].as_array().into_iter().flatten() {
            self.record_under(child, Some(context));
        }
    }

    /// Replaces what is known with a full `browsingContext.getTree` reply and reports the
    /// contexts that are no longer there.
    pub fn sync(&mut self, tree: &Value) -> Removed {
        fn ids(infos: &Value, seen: &mut HashSet<String>) {
            for info in infos.as_array().into_iter().flatten() {
                if let Some(context) = info["context"].as_str() {
                    seen.insert(context.to_string());
                }
                ids(&info["children"], seen);
            }
        }
        let mut seen = HashSet::new();
        ids(&tree["contexts"], &mut seen);
        let mut adopted = false;
        for info in tree["contexts"].as_array().into_iter().flatten() {
            adopted |= self.record(info);
        }
        let gone = self
            .nodes
            .keys()
            .filter(|context| !seen.contains(*context))
            .cloned()
            .collect();
        let mut removed = self.drop_contexts(gone);
        removed.active_changed |= adopted;
        removed
    }

    /// Drops `context` and everything nested in it.
    pub fn remove(&mut self, context: &str) -> Removed {
        let gone = self
            .nodes
            .keys()
            .filter(|id| self.ancestors(id).any(|ancestor| ancestor == context))
            .cloned()
            .collect();
        self.drop_contexts(gone)
    }

    fn drop_contexts(&mut self, gone: HashSet<String>) -> Removed {
        if gone.is_empty() {
            return Removed::default();
        }
        let active_tab = self.active_tab().to_string();
        self.nodes.retain(|context, _| !gone.contains(context));
        let active_changed = gone.contains(&self.active);
        if active_changed {
            self.active = if self.nodes.contains_key(&active_tab) {
                active_tab
            } else {
                self.tabs()
                    .min_by_key(|(_, tab)| tab.id)
                    .map(|(context, _)| context.to_string())
                    .unwrap_or_default()
            };
        }
        Removed {
            contexts: gone,
            active_changed,
        }
    }

    /// `context` followed by its parent chain up to its tab.
    fn ancestors<'a>(&'a self, context: &'a str) -> impl Iterator<Item = &'a str> {
        std::iter::successors(Some(context), |context| match self.nodes.get(*context)? {
            Node::Frame { parent } => Some(parent.as_str()),
            Node::Tab(_) => None,
        })
    }

    /// The context commands act on.
    pub fn active(&self) -> &str {
        &self.active
    }

    /// The tab that holds the active context.
    pub fn active_tab(&self) -> &str {
        self.tab_of(&self.active).unwrap_or(&self.active)
    }

    /// Makes `context` the one commands act on.
    pub fn activate(&mut self, context: &str) {
        self.active = context.to_string();
    }

    /// The tab `context` belongs to: itself for a tab, the top of its parent chain for a frame.
    pub fn tab_of<'a>(&'a self, context: &'a str) -> Option<&'a str> {
        self.ancestors(context)
            .find(|id| matches!(self.nodes.get(*id), Some(Node::Tab(_))))
    }

    /// The record of the tab `context` belongs to.
    pub fn tab(&self, context: &str) -> Option<&Tab> {
        match self.nodes.get(self.tab_of(context)?)? {
            Node::Tab(tab) => Some(tab),
            Node::Frame { .. } => None,
        }
    }

    /// Every tab with its context id.
    pub fn tabs(&self) -> impl Iterator<Item = (&str, &Tab)> {
        self.nodes.iter().filter_map(|(context, node)| match node {
            Node::Tab(tab) => Some((context.as_str(), tab)),
            Node::Frame { .. } => None,
        })
    }

    /// Labels the tab `context` belongs to.
    pub fn set_label(&mut self, context: &str, label: &str) {
        let Some(tab) = self.tab_of(context).map(str::to_string) else {
            return;
        };
        if let Some(Node::Tab(tab)) = self.nodes.get_mut(&tab) {
            tab.label = Some(label.to_string());
        }
    }

    /// The user context of the tab `context` belongs to, when it is not Firefox's default one.
    pub fn user_context(&self, context: &str) -> Option<&str> {
        self.tab(context)
            .map(|tab| tab.user_context.as_str())
            .filter(|user_context| *user_context != DEFAULT_USER_CONTEXT)
    }

    /// The BiDi storage partition of the tab `context` belongs to, when it is not the default one.
    pub fn partition(&self, context: &str) -> Option<Value> {
        self.user_context(context)
            .map(|user_context| json!({"type": "storageKey", "userContext": user_context}))
    }

    /// Notes a user context this session created, so it is removed once no tab uses it.
    pub fn own_user_context(&mut self, user_context: &str) {
        self.owned_user_contexts.insert(user_context.to_string());
    }

    /// Hands over the user contexts this session created that no tab uses any more.
    pub fn take_unused_user_contexts(&mut self) -> Vec<String> {
        let used: HashSet<&str> = self
            .tabs()
            .map(|(_, tab)| tab.user_context.as_str())
            .collect();
        let unused: Vec<String> = self
            .owned_user_contexts
            .iter()
            .filter(|user_context| !used.contains(user_context.as_str()))
            .cloned()
            .collect();
        self.owned_user_contexts
            .retain(|user_context| !unused.contains(user_context));
        unused
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> Value {
        json!({"contexts": [
            {"context": "tab-a", "userContext": "default", "children": [
                {"context": "frame-a", "children": [{"context": "frame-a-inner", "children": []}]},
            ]},
            {"context": "tab-b", "userContext": "jar", "children": []},
        ]})
    }

    #[test]
    fn frames_and_popups_use_the_tab_and_jar_firefox_reports() {
        let mut contexts = Contexts::from_tree(&tree()).unwrap();
        assert_eq!(contexts.tab_of("frame-a-inner"), Some("tab-a"));
        assert_eq!(contexts.partition("frame-a-inner"), None);
        contexts.record(&json!({
            "context": "popup", "parent": null, "userContext": "jar", "originalOpener": "tab-b",
        }));
        contexts.record(&json!({"context": "popup-frame", "parent": "popup"}));
        assert_eq!(contexts.tab("popup").unwrap().id, 3);
        assert_eq!(
            contexts.partition("popup-frame"),
            Some(json!({"type": "storageKey", "userContext": "jar"}))
        );
    }

    #[test]
    fn removing_a_context_drops_what_it_holds_and_moves_the_active_one() {
        let mut contexts = Contexts::from_tree(&tree()).unwrap();
        contexts.activate("frame-a-inner");
        let removed = contexts.remove("frame-a");
        assert!(removed.active_changed);
        assert_eq!(
            removed.contexts,
            HashSet::from(["frame-a".to_string(), "frame-a-inner".to_string()])
        );
        assert_eq!(contexts.active(), "tab-a");
        assert!(contexts.remove("tab-a").active_changed);
        assert_eq!(contexts.active(), "tab-b");
        assert!(!contexts.remove("unknown").active_changed);
        assert!(contexts.remove("tab-b").active_changed);
        assert!(contexts.record(&json!({"context": "tab-d", "parent": null})));
        assert!(!contexts.record(&json!({"context": "tab-e", "parent": null})));
        assert_eq!(contexts.active(), "tab-d");
    }

    #[test]
    fn sync_keeps_tab_ids_and_drops_contexts_missing_from_the_tree() {
        let mut contexts = Contexts::from_tree(&tree()).unwrap();
        contexts.set_label("tab-b", "docs");
        let removed = contexts.sync(&json!({"contexts": [
            {"context": "tab-b", "userContext": "jar", "children": []},
            {"context": "tab-c", "userContext": "default", "children": []},
        ]}));
        assert!(removed.active_changed);
        assert_eq!(removed.contexts.len(), 3);
        assert_eq!(contexts.active(), "tab-b");
        let tab = contexts.tab("tab-b").unwrap();
        assert_eq!((tab.id, tab.label.as_deref()), (2, Some("docs")));
        assert_eq!(contexts.tab("tab-c").unwrap().id, 3);
    }

    #[test]
    fn owned_user_contexts_are_released_once_their_last_tab_is_gone() {
        let mut contexts = Contexts::from_tree(&tree()).unwrap();
        contexts.own_user_context("jar");
        assert!(contexts.take_unused_user_contexts().is_empty());
        contexts.remove("tab-b");
        assert_eq!(contexts.take_unused_user_contexts(), ["jar"]);
        assert!(contexts.take_unused_user_contexts().is_empty());
    }
}
