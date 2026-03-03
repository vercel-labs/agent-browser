#[allow(dead_code)]
pub mod actions;
#[allow(dead_code)]
pub mod auth;
#[allow(dead_code)]
pub mod browser;
#[allow(dead_code)]
pub mod cdp;
#[allow(dead_code)]
pub mod cookies;
#[allow(dead_code)]
pub mod daemon;
#[allow(dead_code)]
pub mod diff;
#[allow(dead_code)]
pub mod element;
#[allow(dead_code)]
pub mod interaction;
#[allow(dead_code)]
pub mod network;
#[allow(dead_code)]
pub mod policy;
#[allow(dead_code)]
pub mod providers;
#[allow(dead_code)]
pub mod recording;
#[allow(dead_code)]
pub mod screenshot;
#[allow(dead_code)]
pub mod snapshot;
#[allow(dead_code)]
pub mod state;
#[allow(dead_code)]
pub mod storage;
#[allow(dead_code)]
pub mod stream;
#[allow(dead_code)]
pub mod tracing;
#[allow(dead_code)]
pub mod webdriver;

#[cfg(test)]
mod e2e_tests;
#[cfg(test)]
mod parity_tests;
