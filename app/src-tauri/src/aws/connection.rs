//! An `SdkConfig` built from a saved `HostConnection`, never from the
//! process's default credentials chain.

use aws_config::identity::IdentityCache;
use aws_config::profile::ProfileFileCredentialsProvider;
use aws_config::provider_config::ProviderConfig;
use aws_config::retry::RetryConfig;
use aws_config::timeout::TimeoutConfig;
use aws_config::{BehaviorVersion, Region, SdkConfig};
use aws_credential_types::provider::error::CredentialsError;
use aws_credential_types::provider::{future, ProvideCredentials, SharedCredentialsProvider};
use aws_credential_types::Credentials;
use aws_runtime::env_config::file::EnvConfigFiles;
use aws_smithy_async::rt::sleep::default_async_sleep;
use aws_smithy_async::time::SharedTimeSource;

use super::error::{AwsError, ProfileCredentialsError};
use crate::models::host_connection::{AwsAuth, HostConnection, HostKind};
use crate::storage::secure;

/// The SDK config for `conn` in `region`. Credentials are resolved lazily,
/// so a missing profile or an expired SSO session surfaces on the first
/// call and is classified there (`classify_sdk_error`).
pub async fn sdk_config(conn: &HostConnection, region: &str) -> Result<SdkConfig, AwsError> {
    sdk_config_with(conn, region, None).await
}

/// `sdk_config` with the profile files overridden (tests pass temp files so
/// neither the process environment nor `~/.aws` is consulted).
pub(crate) async fn sdk_config_with(
    conn: &HostConnection,
    region: &str,
    profile_files: Option<EnvConfigFiles>,
) -> Result<SdkConfig, AwsError> {
    let HostKind::Aws { auth, .. } = &conn.kind;
    let region = Region::new(region.to_string());
    let credentials = match auth {
        AwsAuth::Profile { name } => {
            // The profile is read for credentials only (static keys, SSO,
            // assume-role, credential_process), never for endpoints or
            // service settings.
            let mut builder = ProfileFileCredentialsProvider::builder()
                .configure(&ProviderConfig::default().with_region(Some(region.clone())))
                .profile_name(name);
            if let Some(files) = profile_files {
                builder = builder.profile_files(files);
            }
            SharedCredentialsProvider::new(NamedProfileProvider {
                profile: name.clone(),
                inner: builder.build(),
            })
        }
        AwsAuth::Keys { keyring_ref } => {
            let keys = secure::get_aws_keys(keyring_ref)
                .map_err(|message| AwsError::Other { message })?
                .ok_or(AwsError::KeysMissing)?;
            keys_credentials(&keys)
        }
    };
    Ok(build_sdk_config(region, credentials))
}

/// The SDK config for keys not yet saved (the add-connection check runs
/// before anything touches the keychain).
pub fn sdk_config_for_keys(keys: &secure::AwsKeys, region: &str) -> SdkConfig {
    build_sdk_config(Region::new(region.to_string()), keys_credentials(keys))
}

fn keys_credentials(keys: &secure::AwsKeys) -> SharedCredentialsProvider {
    SharedCredentialsProvider::new(Credentials::new(
        keys.access_key_id.as_str(),
        keys.secret_access_key.as_str(),
        keys.session_token.clone(),
        None,
        "triple-c-connection",
    ))
}

/// Built by hand rather than with `aws_config::defaults(..).load()`, which
/// would read `AWS_ENDPOINT_URL[_<SERVICE>]`, FIPS/dual-stack, retry and
/// other settings from the process environment and the default profile. A
/// stray endpoint there would send every connection's signed requests (and
/// the permission check) somewhere other than AWS. Connections talk to the
/// real regional endpoints; a profile's own `endpoint_url` is ignored too.
/// The HTTP client and stalled-stream protection are each service client's
/// defaults (the `default-https-client` stack every AWS crate here is built
/// with). Timeouts are ours: [`OPERATION_TIMEOUT`] / [`ATTEMPT_TIMEOUT`].
fn build_sdk_config(region: Region, credentials: SharedCredentialsProvider) -> SdkConfig {
    let mut builder = SdkConfig::builder()
        .behavior_version(BehaviorVersion::latest())
        .region(region)
        .credentials_provider(credentials)
        .identity_cache(IdentityCache::lazy().build())
        .retry_config(RetryConfig::standard())
        .timeout_config(timeouts())
        .time_source(SharedTimeSource::default());
    if let Some(sleep) = default_async_sleep() {
        builder = builder.sleep_impl(sleep);
    }
    builder.build()
}

/// Every call the app makes through the SDK is a control-plane call with a
/// small answer (ECS, Secrets Manager, EFS, IAM, SSM, CloudWatch Logs, STS,
/// and S3 reads of state objects capped at 2 KiB); OpenTofu talks to AWS
/// itself, not through these clients. Without a bound a half-open
/// connection would hang a start, a stop or the death watch for minutes.
/// The operation bound covers every retry; the attempt bound is one try.
pub const OPERATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
pub const ATTEMPT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

fn timeouts() -> TimeoutConfig {
    TimeoutConfig::builder()
        .operation_timeout(OPERATION_TIMEOUT)
        .operation_attempt_timeout(ATTEMPT_TIMEOUT)
        .build()
}

/// Puts the profile's name into the source chain of every credentials error,
/// so a failure deep inside the SDK can be reported against the profile.
#[derive(Debug)]
struct NamedProfileProvider {
    profile: String,
    inner: ProfileFileCredentialsProvider,
}

impl NamedProfileProvider {
    async fn load(&self) -> Result<Credentials, CredentialsError> {
        self.inner.provide_credentials().await.map_err(|e| {
            let not_loaded = matches!(e, CredentialsError::CredentialsNotLoaded(_));
            let timed_out = matches!(e, CredentialsError::ProviderTimedOut(_));
            if timed_out {
                return e;
            }
            let invalid = matches!(e, CredentialsError::InvalidConfiguration(_));
            let wrapped = ProfileCredentialsError {
                profile: self.profile.clone(),
                not_loaded,
                source: e,
            };
            // Keep the error's kind; only its context changes.
            if not_loaded {
                CredentialsError::not_loaded(wrapped)
            } else if invalid {
                CredentialsError::invalid_configuration(wrapped)
            } else {
                CredentialsError::provider_error(wrapped)
            }
        })
    }
}

impl ProvideCredentials for NamedProfileProvider {
    fn provide_credentials<'a>(&'a self) -> future::ProvideCredentials<'a>
    where
        Self: 'a,
    {
        future::ProvideCredentials::new(self.load())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aws::identity::caller_identity;
    use crate::aws::AwsError;
    use crate::models::host_connection::{AwsAuth, HostConnection, HostKind};
    use aws_runtime::env_config::file::{EnvConfigFileKind, EnvConfigFiles};

    fn profile_conn(name: &str) -> HostConnection {
        HostConnection {
            id: "c1".into(),
            name: "Test".into(),
            kind: HostKind::Aws {
                auth: AwsAuth::Profile { name: name.into() },
                default_region: "us-west-2".into(),
            },
        }
    }

    /// Empty config and credentials files, passed explicitly so neither the
    /// process environment nor ~/.aws is consulted.
    #[tokio::test]
    async fn a_missing_profile_is_reported_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        let creds = dir.path().join("credentials");
        std::fs::write(&config, "").unwrap();
        std::fs::write(&creds, "").unwrap();
        let files = EnvConfigFiles::builder()
            .include_default_config_file(false)
            .include_default_credentials_file(false)
            .with_file(EnvConfigFileKind::Config, &config)
            .with_file(EnvConfigFileKind::Credentials, &creds)
            .build();
        let cfg = sdk_config_with(&profile_conn("nope"), "us-west-2", Some(files))
            .await
            .unwrap();
        let err = caller_identity(&cfg).await.unwrap_err();
        assert_eq!(
            err,
            AwsError::ProfileNotFound {
                profile: "nope".into()
            }
        );
    }

    /// A profile that the files do define but under another name is still
    /// "not found" for the name the connection asked for.
    #[tokio::test]
    async fn a_profile_absent_from_populated_files_is_reported_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        std::fs::write(
            &config,
            "[profile other]\naws_access_key_id = AKIDTEST\naws_secret_access_key = s\n",
        )
        .unwrap();
        let files = EnvConfigFiles::builder()
            .include_default_config_file(false)
            .include_default_credentials_file(false)
            .with_file(EnvConfigFileKind::Config, &config)
            .build();
        let cfg = sdk_config_with(&profile_conn("nope"), "us-west-2", Some(files))
            .await
            .unwrap();
        let err = caller_identity(&cfg).await.unwrap_err();
        assert_eq!(
            err,
            AwsError::ProfileNotFound {
                profile: "nope".into()
            }
        );
    }

    #[tokio::test]
    async fn the_region_comes_from_the_argument() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        std::fs::write(&config, "[profile p]\nregion = eu-west-1\n").unwrap();
        let files = EnvConfigFiles::builder()
            .include_default_config_file(false)
            .include_default_credentials_file(false)
            .with_file(EnvConfigFileKind::Config, &config)
            .build();
        let cfg = sdk_config_with(&profile_conn("p"), "ap-south-1", Some(files))
            .await
            .unwrap();
        assert_eq!(cfg.region().map(|r| r.as_ref()), Some("ap-south-1"));
    }

    /// Config files that try to redirect every call. A connection always
    /// talks to the real regional AWS endpoints, so none of this may reach
    /// the config: not the named profile's `endpoint_url`, not `[default]`'s,
    /// not a per-service `services` section.
    const REDIRECTING_CONFIG: &str = "[default]
endpoint_url = http://127.0.0.1:9
retry_mode = adaptive
max_attempts = 9
use_fips_endpoint = true

[profile p]
endpoint_url = http://127.0.0.1:9
retry_mode = adaptive
max_attempts = 9
use_dualstack_endpoint = true
services = redirect
aws_access_key_id = AKIDTEST
aws_secret_access_key = s

[services redirect]
sts =
  endpoint_url = http://127.0.0.1:9
";

    fn assert_isolated(cfg: &aws_config::SdkConfig) {
        assert_eq!(cfg.endpoint_url(), None);
        assert!(
            cfg.service_config().is_none(),
            "service config loaded from env/profile"
        );
        assert_eq!(cfg.use_fips(), None);
        assert_eq!(cfg.use_dual_stack(), None);
        let retry = cfg.retry_config().expect("retry config");
        assert_eq!(retry.mode(), aws_config::retry::RetryMode::Standard);
        assert_eq!(retry.max_attempts(), 3);
        assert_eq!(cfg.region().map(|r| r.as_ref()), Some("us-west-2"));
    }

    #[tokio::test]
    async fn a_profile_connection_ignores_endpoint_and_service_settings_in_the_profile() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        std::fs::write(&config, REDIRECTING_CONFIG).unwrap();
        let files = EnvConfigFiles::builder()
            .include_default_config_file(false)
            .include_default_credentials_file(false)
            .with_file(EnvConfigFileKind::Config, &config)
            .build();
        let cfg = sdk_config_with(&profile_conn("p"), "us-west-2", Some(files))
            .await
            .unwrap();
        assert_isolated(&cfg);
    }

    /// The keys arm reads no files or environment at all: same isolation.
    #[test]
    fn a_keys_connection_config_is_isolated() {
        let keys = crate::storage::secure::AwsKeys {
            access_key_id: "AKIDTEST".into(),
            secret_access_key: "s".into(),
            session_token: None,
        };
        let cfg = build_sdk_config(Region::new("us-west-2"), keys_credentials(&keys));
        assert_isolated(&cfg);
        assert!(cfg.credentials_provider().is_some());
    }

    #[test]
    fn every_sdk_call_is_bounded() {
        let keys = crate::storage::secure::AwsKeys {
            access_key_id: "AKIDTEST".into(),
            secret_access_key: "s".into(),
            session_token: None,
        };
        let cfg = build_sdk_config(Region::new("us-west-2"), keys_credentials(&keys));
        let t = cfg.timeout_config().expect("a timeout config");
        assert_eq!(t.operation_timeout(), Some(OPERATION_TIMEOUT));
        assert_eq!(t.operation_attempt_timeout(), Some(ATTEMPT_TIMEOUT));
        assert!(ATTEMPT_TIMEOUT < OPERATION_TIMEOUT);
    }
}
