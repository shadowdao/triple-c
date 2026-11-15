//! AWS connection plumbing: an SDK config built from a saved
//! `HostConnection`, the caller's identity, the tier policies and the
//! permission check.
//!
//! Nothing outside this module calls in yet: the Tauri commands (Task 6)
//! will. Until then the entry points they call carry `#[allow(dead_code)]`
//! (which keeps everything they reach live) and the re-exports below carry
//! `unused_imports`; no blanket allow.

mod connection;
mod error;
mod identity;
pub mod permissions;
pub mod policies;

#[allow(unused_imports)]
pub use connection::sdk_config;
#[allow(unused_imports)]
pub use error::{classify_sdk_error, AwsError};
#[allow(unused_imports)]
pub use identity::{
    caller_identity, principal_for_simulation, resolve_principal, CallerIdentity, PrincipalArn,
};
