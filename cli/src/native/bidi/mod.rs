//! WebDriver BiDi engine support. Firefox is the implemented engine; Firefox-only DevTools code lives in `firefox.rs` and `firefox_devtools.rs`.

pub mod actions;
pub mod capture;
pub mod client;
pub mod contexts;
pub mod daemon;
pub mod emulation;
pub mod firefox;
pub mod firefox_devtools;
pub mod forms;
pub mod input;
pub mod interact;
pub mod navigation;
pub mod network;
pub mod page;
pub mod permissions;
pub mod query;
pub mod session;
pub mod snapshot;
pub mod storage;
pub mod stream;
pub mod tabs;
pub mod target;
pub mod types;
