#[cfg(feature = "cli")]
pub mod cli;
pub mod config;
pub mod control;
pub mod crypto;
pub mod delta;
pub mod discovery;
mod encoding;
pub mod error;
pub mod filesystem;
pub mod net;
pub mod protocol;
mod receiver;
mod reconcile;
pub mod reporter;
pub mod secure_store;
mod storage;
mod sync;
pub mod transfer;
pub mod update;
pub mod version;
pub mod workflow;

#[cfg(feature = "cli")]
pub use cli::run;
pub use version::VERSION;
