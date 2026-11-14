//! An `SdkConfig` built from a saved `HostConnection`, never from the
//! process's default credentials chain.

use aws_config::profile::ProfileFileCredentialsProvider;
use aws_config::provider_config::ProviderConfig;
use aws_config::{BehaviorVersion, Region, SdkConfig};
use aws_credential_types::provider::error::CredentialsError;
use aws_credential_types::provider::{future, ProvideCredentials};
use aws_credential_types::Credentials;
use aws_runtime::env_config::file::EnvConfigFiles;

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
    let mut loader = aws_config::defaults(BehaviorVersion::latest()).region(region.clone());
    match auth {
        AwsAuth::Profile { name } => {
            let mut builder = ProfileFileCredentialsProvider::builder()
                .configure(&ProviderConfig::default().with_region(Some(region)))
                .profile_name(name);
            if let Some(files) = &profile_files {
                builder = builder.profile_files(files.clone());
                loader = loader.profile_files(files.clone());
            }
            // Non-credential settings (retry mode, endpoint overrides) come
            // from the same profile the credentials do.
            loader = loader
                .profile_name(name)
                .credentials_provider(NamedProfileProvider {
                    profile: name.clone(),
                    inner: builder.build(),
                });
        }
        AwsAuth::Keys { keyring_ref } => {
            let keys = secure::get_aws_keys(keyring_ref)
                .map_err(|message| AwsError::Other { message })?
                .ok_or(AwsError::KeysMissing)?;
            loader = loader.credentials_provider(Credentials::new(
                keys.access_key_id.as_str(),
                keys.secret_access_key.as_str(),
                keys.session_token.clone(),
                None,
                "triple-c-connection",
            ));
        }
    }
    Ok(loader.load().await)
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
}
