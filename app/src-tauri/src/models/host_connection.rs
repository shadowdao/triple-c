//! Remote host connections (currently AWS). Only non-secret data lives here;
//! access keys are held in the OS keychain and referenced by `keyring_ref`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostConnection {
    pub id: String,
    pub name: String,
    pub kind: HostKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostKind {
    Aws { auth: AwsAuth, default_region: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum AwsAuth {
    /// A named profile from the host's AWS config.
    Profile { name: String },
    /// Static keys stored in the OS keychain under this reference.
    Keys { keyring_ref: String },
}

/// Trimmed, 1..=64 characters, no control characters.
pub fn validate_connection_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    let len = name.chars().count();
    if len == 0 || len > 64 {
        return Err("Connection name must be 1 to 64 characters".into());
    }
    if name.chars().any(char::is_control) {
        return Err("Connection name must not contain control characters".into());
    }
    Ok(name.to_string())
}

/// An AWS region id such as `us-west-2` or `us-gov-west-1`.
pub fn validate_region(region: &str) -> Result<String, String> {
    let b = region.as_bytes();
    let ok = (|| {
        // ^[a-z]{2}(-[a-z]+)+-\d+$
        if b.len() < 2 || !b[..2].iter().all(u8::is_ascii_lowercase) {
            return false;
        }
        let mut parts = region[2..].split('-');
        if parts.next() != Some("") {
            return false;
        }
        let rest: Vec<&str> = parts.collect();
        let Some((last, words)) = rest.split_last() else {
            return false;
        };
        !words.is_empty()
            && words
                .iter()
                .all(|w| !w.is_empty() && w.bytes().all(|c| c.is_ascii_lowercase()))
            && !last.is_empty()
            && last.bytes().all(|c| c.is_ascii_digit())
    })();
    if ok {
        Ok(region.to_string())
    } else {
        Err(format!("\"{}\" is not a valid AWS region", region.escape_debug()))
    }
}

/// Non-empty, at most 128 characters of `[A-Za-z0-9_.@+=,-]`.
pub fn validate_profile_name(name: &str) -> Result<String, String> {
    if name.is_empty() || name.len() > 128 {
        return Err("Profile name must be 1 to 128 characters".into());
    }
    if !name
        .bytes()
        .all(|c| c.is_ascii_alphanumeric() || b"_.@+=,-".contains(&c))
    {
        return Err("Profile name contains characters AWS profiles do not allow".into());
    }
    Ok(name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::AppSettings;

    #[test]
    fn settings_without_hosts_deserialize_to_empty() {
        let mut json = serde_json::to_value(AppSettings::default()).unwrap();
        json.as_object_mut().unwrap().remove("hosts");
        let s: AppSettings = serde_json::from_value(json).unwrap();
        assert_eq!(s.hosts, vec![]);
    }

    #[test]
    fn a_profile_connection_round_trips() {
        let c = HostConnection {
            id: "h1".into(),
            name: "Prod".into(),
            kind: HostKind::Aws {
                auth: AwsAuth::Profile { name: "default".into() },
                default_region: "us-west-2".into(),
            },
        };
        let s = serde_json::to_string(&c).unwrap();
        assert!(s.contains("\"type\":\"aws\""), "{s}");
        assert!(s.contains("\"method\":\"profile\""), "{s}");
        assert_eq!(serde_json::from_str::<HostConnection>(&s).unwrap(), c);
    }

    #[test]
    fn regions_are_validated() {
        for ok in ["us-west-2", "eu-central-1", "cn-north-1", "us-gov-west-1"] {
            assert_eq!(validate_region(ok).unwrap(), ok);
        }
        for bad in ["", "US-WEST-2", "us-west", "us-west-2; rm"] {
            assert!(validate_region(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn profile_names_are_validated() {
        for ok in ["default", "340315884407_StaffAdministratorAccess"] {
            assert_eq!(validate_profile_name(ok).unwrap(), ok);
        }
        for bad in ["", "a b", "x\n"] {
            assert!(validate_profile_name(bad).is_err(), "{bad:?}");
        }
        assert!(validate_profile_name(&"a".repeat(129)).is_err());
        assert!(validate_profile_name(&"a".repeat(128)).is_ok());
    }

    #[test]
    fn connection_names_are_validated() {
        assert_eq!(validate_connection_name("  Prod  ").unwrap(), "Prod");
        assert!(validate_connection_name("   ").is_err());
        assert!(validate_connection_name("a\tb").is_err());
        assert!(validate_connection_name(&"a".repeat(65)).is_err());
    }
}
