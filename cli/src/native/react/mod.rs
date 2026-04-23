//! React/web introspection primitives.
//!
//! Scripts and handlers for the `react` subcommands (tree, inspect, renders,
//! suspense) plus the universal `vitals` verb and the generic `pushstate`
//! SPA-navigation action. These primitives are framework-agnostic: React-side
//! commands only require the `__REACT_DEVTOOLS_GLOBAL_HOOK__` to be installed,
//! and `vitals` / `pushstate` are pure web-standard APIs.
//!
//! The React DevTools `installHook.js` is vendored from the React DevTools
//! Chrome extension (MIT, facebook/react). It's registered via
//! `addScriptToEvaluateOnNewDocument` before any page JS runs when the user
//! passes `--enable react-devtools` at launch.

pub mod scripts;

mod renders;
mod suspense;
mod tree;
mod vitals;

pub use renders::{format_renders_report, RendersData};
pub use suspense::{format_suspense_report, Boundary};
pub use tree::{format_tree, TreeNode};
pub use vitals::{format_vitals_report, VitalsData};

/// React DevTools hook script (MIT, from facebook/react).
/// Registered via `addScriptToEvaluateOnNewDocument` to install
/// `window.__REACT_DEVTOOLS_GLOBAL_HOOK__` before any page JS runs. React
/// detects the hook on boot and registers its renderers against it, which
/// enables every `react …` command.
pub const INSTALL_HOOK_JS: &str = include_str!("installHook.js");
