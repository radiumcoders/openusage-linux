//! OpenUsage for Linux: the accounts, sign-ins, providers, and refresh logic of the Mac app, in Rust.

pub mod account;
pub mod accounts;
pub mod claims;
pub mod daemon;
pub mod dashboard;
pub mod descriptor;
pub mod engine;
pub mod error;
pub mod http;
pub mod hub;
pub mod kind;
pub mod layout;
pub mod layout_v1;
pub mod log;
pub mod login;
pub mod metric;
pub mod paths;
pub mod provider;
pub mod providers;
pub mod proxy;
pub mod refresh;
pub mod settings;
pub mod store;
pub mod vault;
pub mod widget;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
