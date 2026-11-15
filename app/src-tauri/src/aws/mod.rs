//! AWS connection plumbing: an SDK config built from a saved
//! `HostConnection`, the caller's identity, the tier policies and the
//! permission check.
//!

mod connection;
pub mod environments;
mod error;
mod identity;
#[cfg(test)]
mod live_tests;
pub mod permissions;
pub mod policies;
#[cfg(test)]
mod policy_eval;
pub mod runtime;
pub mod tasks;
pub mod tofu;
pub mod tofu_modules;
pub mod workspace;

pub use connection::{sdk_config, sdk_config_for_keys};
pub use error::{classify_sdk_error, AwsError};
pub use identity::{caller_identity, CallerIdentity};
