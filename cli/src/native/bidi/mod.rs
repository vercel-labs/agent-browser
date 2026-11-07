//! WebDriver BiDi engine support. Firefox is the implemented engine; Firefox-only DevTools code lives in `firefox.rs` and `firefox_devtools.rs`.
#![allow(dead_code)]

pub mod actions;
pub mod attach;
pub mod capture;
pub mod client;
pub mod emulation;
pub mod firefox;
pub mod firefox_devtools;
pub mod forms;
pub mod input;
pub mod interact;
pub mod network;
pub mod page;
pub mod query;
pub mod session;
pub mod snapshot;
pub mod storage;
pub mod stream;
pub mod tabs;
pub mod target;
pub mod types;
