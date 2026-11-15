//! What can go wrong talking to AWS, in the terms the UI acts on.

use std::error::Error;

use aws_sdk_sts::error::{DisplayErrorContext, ErrorMetadata};
use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AwsError {
    /// The UI offers "Sign in".
    SsoExpired {
        profile: String,
    },
    ProfileNotFound {
        profile: String,
    },
    /// The keychain entry for a keys connection is gone.
    KeysMissing,
    InvalidCredentials {
        message: String,
    },
    AccessDenied {
        action: String,
        message: String,
    },
    /// `s3:CreateBucket` said `BucketAlreadyExists`: the state bucket's
    /// name belongs to another AWS account. Never treated as success.
    StateBucketTaken {
        bucket: String,
    },
    Other {
        message: String,
    },
}

impl std::fmt::Display for AwsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AwsError::SsoExpired { profile } if profile.is_empty() => {
                write!(f, "The AWS SSO session has expired; sign in again")
            }
            AwsError::SsoExpired { profile } => write!(
                f,
                "The AWS SSO session for profile \"{profile}\" has expired; sign in again"
            ),
            AwsError::ProfileNotFound { profile } => write!(
                f,
                "AWS profile \"{profile}\" was not found, or has no credentials, in the AWS config and credentials files"
            ),
            AwsError::KeysMissing => write!(
                f,
                "The access keys for this connection are no longer in the system keychain; enter them again"
            ),
            AwsError::InvalidCredentials { message } => {
                write!(f, "AWS rejected the credentials: {message}")
            }
            AwsError::AccessDenied { action, message } => {
                write!(f, "Access denied for {action}: {message}")
            }
            AwsError::StateBucketTaken { bucket } => write!(
                f,
                "The state bucket name \"{bucket}\" is already taken by another AWS account, so \
                 Triple-C can't keep OpenTofu state there"
            ),
            AwsError::Other { message } => f.write_str(message),
        }
    }
}

/// Carried in the source chain of every credentials error from a profile
/// connection, so classification knows which profile failed and whether it
/// produced no credentials at all (absent, or present without credentials).
#[derive(Debug)]
pub(crate) struct ProfileCredentialsError {
    pub profile: String,
    pub not_loaded: bool,
    pub source: aws_credential_types::provider::error::CredentialsError,
}

impl std::fmt::Display for ProfileCredentialsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "credentials for profile `{}`", self.profile)
    }
}

impl Error for ProfileCredentialsError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.source)
    }
}

const SSO_EXPIRED: &[&str] = &[
    "The SSO session associated with this profile has expired",
    "Token has expired",
    "the SSO token has expired",
];
const SSO_REVOKED: &[&str] = &[
    "UnauthorizedException",
    "Session token not found or invalid",
];
const DENIED_CODES: &[&str] = &[
    "AccessDeniedException",
    "AccessDenied",
    "UnauthorizedOperation",
];
const BAD_CREDENTIAL_CODES: &[&str] = &[
    "ExpiredToken",
    "InvalidClientTokenId",
    "SignatureDoesNotMatch",
    "UnrecognizedClientException",
];

/// Classify any SDK error (an `SdkError`, a credentials error, ...) from
/// calling `action` (e.g. `iam:GetRole`). Matching is on the full display
/// chain, which carries both provider messages and service error codes.
pub fn classify_sdk_error(action: &str, err: &(dyn Error + 'static)) -> AwsError {
    let text = DisplayErrorContext(err).to_string();
    let mut chain = std::iter::successors(Some(err), |e| (*e).source());
    let profile = chain
        .clone()
        .find_map(|e| e.downcast_ref::<ProfileCredentialsError>());
    // The service's own message, when one is in the chain.
    let service_message = chain
        .find_map(|e| e.downcast_ref::<ErrorMetadata>())
        .and_then(|m| m.message())
        .map(str::to_string);
    let message = service_message.unwrap_or_else(|| text.clone());

    if SSO_EXPIRED.iter().any(|s| text.contains(s)) {
        return AwsError::SsoExpired {
            profile: profile.map(|p| p.profile.clone()).unwrap_or_default(),
        };
    }
    // SSO's GetRoleCredentials on a revoked or signed-out session. Only
    // inside a profile's credentials: other services use this code too.
    if let Some(p) = profile.filter(|_| SSO_REVOKED.iter().any(|s| text.contains(s))) {
        return AwsError::SsoExpired {
            profile: p.profile.clone(),
        };
    }
    if let Some(p) = profile.filter(|p| p.not_loaded) {
        return AwsError::ProfileNotFound {
            profile: p.profile.clone(),
        };
    }
    if DENIED_CODES.iter().any(|c| text.contains(c)) {
        return AwsError::AccessDenied {
            action: action.to_string(),
            message,
        };
    }
    if BAD_CREDENTIAL_CODES.iter().any(|c| text.contains(c)) {
        return AwsError::InvalidCredentials { message };
    }
    AwsError::Other { message: text }
}
