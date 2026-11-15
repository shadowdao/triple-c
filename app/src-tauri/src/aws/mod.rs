//! AWS connection plumbing: an SDK config built from a saved
//! `HostConnection`, the caller's identity, and error classification.

// The permission check (Task 5) and the Tauri commands (Task 6) wire these in.
#![allow(dead_code, unused_imports)]

mod connection;
mod error;
mod identity;
pub mod policies;

pub use connection::sdk_config;
pub use error::{classify_sdk_error, AwsError};
pub use identity::{
    caller_identity, principal_for_simulation, resolve_principal, CallerIdentity, PrincipalArn,
};
