//! AWS connection plumbing: an SDK config built from a saved
//! `HostConnection`, the caller's identity, the tier policies and the
//! permission check.
//!

mod connection;
mod error;
mod identity;
#[cfg(test)]
mod live_tests;
pub mod permissions;
pub mod policies;
pub mod tofu_modules;

pub use connection::{sdk_config, sdk_config_for_keys};
pub use error::AwsError;
pub use identity::{caller_identity, CallerIdentity};
