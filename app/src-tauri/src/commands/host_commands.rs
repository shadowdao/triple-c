//! Commands for host connections (AWS): add, update, remove, identity,
//! sign-in and the permission check.

use std::future::Future;

use tauri::State;

use crate::aws::permissions::{check_permissions, PermissionReport, SdkPermissionApi};
use crate::aws::policies::{policy_files, Tier};
use crate::aws::{caller_identity, sdk_config, sdk_config_for_keys, AwsError, CallerIdentity};
use crate::models::{
    validate_connection_name, validate_profile_name, validate_supported_region, AppSettings, AwsAuth,
    HostConnection, HostKind,
};
use crate::storage::secure::{self, AwsKeys};
use crate::storage::settings_store::SettingsStore;
use crate::AppState;

fn other(message: impl Into<String>) -> AwsError {
    AwsError::Other { message: message.into() }
}

fn check_id(id: &str) -> Result<(), String> {
    if secure::is_valid_connection_id(id) {
        Ok(())
    } else {
        Err("Invalid connection id".to_string())
    }
}

/// What the identity check is asked to authenticate with.
pub(crate) enum Credentials {
    /// A named host profile (the connection itself).
    Profile(HostConnection),
    /// Keys not yet in the keychain, and the region to call in.
    Keys(AwsKeys, String),
}

/// Validate-free core of both add commands: `GetCallerIdentity` first, and
/// only on success keys are stored and the connection saved. An expired SSO
/// session on a profile still saves (the UI then offers "Sign in").
pub(crate) async fn add_connection_with<I, Fut>(
    conn: HostConnection,
    keys: Option<AwsKeys>,
    identity: I,
    store_keys: impl Fn(&str, &AwsKeys) -> Result<(), String>,
    delete_keys: impl Fn(&str) -> Result<(), String>,
    settings: &SettingsStore,
) -> Result<HostConnection, AwsError>
where
    I: FnOnce(Credentials) -> Fut,
    Fut: Future<Output = Result<CallerIdentity, AwsError>>,
{
    let HostKind::Aws { auth, default_region } = &conn.kind;
    let credentials = match (&keys, auth) {
        (Some(k), AwsAuth::Keys { .. }) => Credentials::Keys(k.clone(), default_region.clone()),
        (None, AwsAuth::Profile { .. }) => Credentials::Profile(conn.clone()),
        _ => return Err(other("Connection credentials do not match its auth method")),
    };
    match identity(credentials).await {
        Ok(_) => {}
        Err(AwsError::SsoExpired { .. }) if keys.is_none() => {}
        Err(e) => return Err(e),
    }

    if let Some(k) = &keys {
        store_keys(&conn.id, k).map_err(other)?;
    }
    if let Err(e) = settings.modify(|all| {
        all.hosts.push(conn.clone());
        Ok(())
    }) {
        if keys.is_some() {
            let _ = delete_keys(&conn.id);
        }
        return Err(other(e));
    }
    Ok(conn)
}

/// Removes the stored connection with this id, however malformed its other
/// fields are (a broken entry must stay removable). Its keychain entry is
/// deleted only for a `Keys` connection whose `keyring_ref` is its own valid id.
pub(crate) fn remove_connection_with(
    id: &str,
    delete_keys: impl Fn(&str) -> Result<(), String>,
    settings: &SettingsStore,
) -> Result<AppSettings, String> {
    check_id(id)?;
    let existing = settings
        .get()
        .hosts
        .into_iter()
        .find(|h| h.id == id)
        .ok_or_else(|| "Connection not found".to_string())?;
    let HostKind::Aws { auth, .. } = &existing.kind;
    if let AwsAuth::Keys { keyring_ref } = auth {
        if keyring_ref == id && secure::is_valid_connection_id(keyring_ref) {
            delete_keys(keyring_ref)?;
        }
    }
    settings
        .modify(|s| {
            s.hosts.retain(|h| h.id != id);
            Ok(())
        })
        .map(|(saved, ())| saved)
}

async fn real_identity(c: Credentials) -> Result<CallerIdentity, AwsError> {
    let cfg = match c {
        Credentials::Profile(conn) => {
            let HostKind::Aws { default_region, .. } = &conn.kind;
            sdk_config(&conn, default_region).await?
        }
        Credentials::Keys(keys, region) => sdk_config_for_keys(&keys, &region),
    };
    caller_identity(&cfg).await
}

pub(crate) fn aws_conn(id: &str, settings: &AppSettings) -> Result<HostConnection, String> {
    check_id(id)?;
    let conn = settings
        .hosts
        .iter()
        .find(|h| h.id == id)
        .cloned()
        .ok_or_else(|| "Connection not found".to_string())?;
    // Defense in depth: stored values are re-checked before they reach the
    // keychain, the SDK or a subprocess.
    let HostKind::Aws { auth, default_region } = &conn.kind;
    validate_supported_region(default_region)?;
    match auth {
        AwsAuth::Profile { name } => {
            validate_profile_name(name)?;
        }
        AwsAuth::Keys { keyring_ref } if keyring_ref != id => {
            return Err("Connection's keychain reference does not match its id".to_string());
        }
        AwsAuth::Keys { .. } => {}
    }
    Ok(conn)
}

fn validated(name: &str, region: &str) -> Result<(String, String), AwsError> {
    Ok((
        validate_connection_name(name).map_err(other)?,
        validate_supported_region(region).map_err(other)?,
    ))
}

#[tauri::command]
pub async fn add_aws_profile_connection(
    name: String,
    profile: String,
    default_region: String,
    state: State<'_, AppState>,
) -> Result<HostConnection, AwsError> {
    let (name, default_region) = validated(&name, &default_region)?;
    let profile = validate_profile_name(&profile).map_err(other)?;
    let conn = HostConnection {
        id: uuid::Uuid::new_v4().to_string(),
        name,
        kind: HostKind::Aws { auth: AwsAuth::Profile { name: profile }, default_region },
    };
    add_connection_with(
        conn,
        None,
        real_identity,
        secure::store_aws_keys,
        secure::delete_aws_keys,
        &state.settings_store,
    )
    .await
}

#[tauri::command]
pub async fn add_aws_keys_connection(
    name: String,
    access_key_id: String,
    secret_access_key: String,
    session_token: Option<String>,
    default_region: String,
    state: State<'_, AppState>,
) -> Result<HostConnection, AwsError> {
    let (name, default_region) = validated(&name, &default_region)?;
    let access_key_id = access_key_id.trim().to_string();
    let secret_access_key = secret_access_key.trim().to_string();
    let session_token = session_token.map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
    if access_key_id.is_empty() || secret_access_key.is_empty() {
        return Err(other("Access key ID and secret access key are required"));
    }
    let id = uuid::Uuid::new_v4().to_string();
    let conn = HostConnection {
        id: id.clone(),
        name,
        kind: HostKind::Aws { auth: AwsAuth::Keys { keyring_ref: id }, default_region },
    };
    let keys = AwsKeys { access_key_id, secret_access_key, session_token };
    add_connection_with(
        conn,
        Some(keys),
        real_identity,
        secure::store_aws_keys,
        secure::delete_aws_keys,
        &state.settings_store,
    )
    .await
}

#[tauri::command]
pub async fn update_aws_connection(
    id: String,
    name: String,
    default_region: String,
    state: State<'_, AppState>,
) -> Result<HostConnection, AwsError> {
    let (name, region) = validated(&name, &default_region)?;
    let (_, updated) = state
        .settings_store
        .modify(|settings| {
            aws_conn(&id, settings)?;
            let conn = settings.hosts.iter_mut().find(|h| h.id == id).expect("checked above");
            conn.name = name;
            let HostKind::Aws { default_region, .. } = &mut conn.kind;
            *default_region = region;
            Ok(conn.clone())
        })
        .map_err(other)?;
    Ok(updated)
}

#[tauri::command]
pub async fn remove_host_connection(
    id: String,
    state: State<'_, AppState>,
) -> Result<AppSettings, String> {
    remove_connection_with(&id, secure::delete_aws_keys, &state.settings_store)
}

#[tauri::command]
pub async fn aws_connection_identity(
    id: String,
    state: State<'_, AppState>,
) -> Result<CallerIdentity, AwsError> {
    let conn = aws_conn(&id, &state.settings_store.get()).map_err(other)?;
    let HostKind::Aws { default_region, .. } = &conn.kind;
    let cfg = sdk_config(&conn, default_region).await?;
    caller_identity(&cfg).await
}

#[tauri::command]
pub async fn aws_connection_sso_login(
    id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let conn = aws_conn(&id, &state.settings_store.get())?;
    let HostKind::Aws { auth, .. } = &conn.kind;
    let AwsAuth::Profile { name } = auth else {
        return Err("Only profile connections use SSO".to_string());
    };
    let name = validate_profile_name(name)?;
    super::aws_commands::run_sso_login(&name).await
}

#[tauri::command]
pub async fn check_aws_permissions(
    id: String,
    region: Option<String>,
    state: State<'_, AppState>,
) -> Result<PermissionReport, AwsError> {
    let conn = aws_conn(&id, &state.settings_store.get()).map_err(other)?;
    let HostKind::Aws { default_region, .. } = &conn.kind;
    let region = match region {
        Some(r) => validate_supported_region(&r).map_err(other)?,
        None => default_region.clone(),
    };
    let cfg = sdk_config(&conn, &region).await?;
    let identity = caller_identity(&cfg).await?;
    let api = SdkPermissionApi::new(&cfg);
    check_permissions(&api, &identity, &region).await
}

/// One policy file of a tier: its file name under `aws/policies/` and its
/// JSON, verbatim.
#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct PolicyFile {
    pub name: String,
    pub json: String,
}

/// Every policy file `tier` needs, in order; all of them are required (D7).
#[tauri::command]
pub fn aws_policy_document(tier: Tier) -> Vec<PolicyFile> {
    policy_files(tier)
        .iter()
        .map(|(name, json)| PolicyFile {
            name: name.to_string(),
            json: json.to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn profile_conn() -> HostConnection {
        HostConnection {
            id: "c1".into(),
            name: "Prod".into(),
            kind: HostKind::Aws {
                auth: AwsAuth::Profile { name: "dev".into() },
                default_region: "us-west-2".into(),
            },
        }
    }

    fn keys_conn() -> HostConnection {
        HostConnection {
            id: "c2".into(),
            name: "K".into(),
            kind: HostKind::Aws {
                auth: AwsAuth::Keys { keyring_ref: "c2".into() },
                default_region: "us-west-2".into(),
            },
        }
    }

    fn keys() -> AwsKeys {
        AwsKeys { access_key_id: "AKIA".into(), secret_access_key: "s".into(), session_token: None }
    }

    fn ident() -> CallerIdentity {
        CallerIdentity { account: "1".into(), arn: "arn:aws:iam::1:user/u".into(), user_id: "U".into() }
    }

    fn store_in(dir: &tempfile::TempDir) -> SettingsStore {
        SettingsStore::at_path(dir.path().join("settings.json"))
    }

    #[tokio::test]
    async fn adding_saves_nothing_when_identity_fails() {
        let dir = tempfile::tempdir().unwrap();
        let settings = store_in(&dir);
        let calls = RefCell::new(Vec::<String>::new());
        let err = add_connection_with(
            keys_conn(),
            Some(keys()),
            |_| async { Err(AwsError::InvalidCredentials { message: "bad".into() }) },
            |id, _| { calls.borrow_mut().push(format!("store {id}")); Ok(()) },
            |id| { calls.borrow_mut().push(format!("delete {id}")); Ok(()) },
            &settings,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AwsError::InvalidCredentials { .. }));
        assert!(settings.get().hosts.is_empty());
        assert!(!dir.path().join("settings.json").exists());
        assert!(calls.borrow().is_empty(), "{:?}", calls.borrow());
    }

    #[tokio::test]
    async fn an_sso_expired_profile_is_still_saved() {
        let dir = tempfile::tempdir().unwrap();
        let settings = store_in(&dir);
        let saved = add_connection_with(
            profile_conn(),
            None,
            |_| async { Err(AwsError::SsoExpired { profile: "dev".into() }) },
            |_, _| Ok(()),
            |_| Ok(()),
            &settings,
        )
        .await
        .unwrap();
        assert_eq!(settings.get().hosts, vec![saved]);
    }

    #[tokio::test]
    async fn a_successful_keys_add_stores_keys_then_settings() {
        let dir = tempfile::tempdir().unwrap();
        let settings = store_in(&dir);
        let calls = RefCell::new(Vec::<String>::new());
        let saved = add_connection_with(
            keys_conn(),
            Some(keys()),
            |c| async move {
                assert!(matches!(c, Credentials::Keys(..)));
                Ok(ident())
            },
            |id, _| { calls.borrow_mut().push(format!("store {id}")); Ok(()) },
            |id| { calls.borrow_mut().push(format!("delete {id}")); Ok(()) },
            &settings,
        )
        .await
        .unwrap();
        assert_eq!(*calls.borrow(), vec!["store c2".to_string()]);
        assert_eq!(settings.get().hosts, vec![saved]);
    }

    #[tokio::test]
    async fn keys_are_removed_if_saving_settings_fails() {
        let dir = tempfile::tempdir().unwrap();
        // The parent directory does not exist, so saving fails.
        let settings = SettingsStore::at_path(dir.path().join("missing/settings.json"));
        let calls = RefCell::new(Vec::<String>::new());
        let err = add_connection_with(
            keys_conn(),
            Some(keys()),
            |_| async { Ok(ident()) },
            |id, _| { calls.borrow_mut().push(format!("store {id}")); Ok(()) },
            |id| { calls.borrow_mut().push(format!("delete {id}")); Ok(()) },
            &settings,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AwsError::Other { .. }));
        assert_eq!(*calls.borrow(), vec!["store c2".to_string(), "delete c2".to_string()]);
        assert!(settings.get().hosts.is_empty(), "no unsaved connection in memory");
    }

    #[tokio::test]
    async fn a_non_sso_identity_error_on_a_profile_saves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let settings = store_in(&dir);
        let err = add_connection_with(
            profile_conn(),
            None,
            |_| async { Err(AwsError::ProfileNotFound { profile: "dev".into() }) },
            |_, _| Ok(()),
            |_| Ok(()),
            &settings,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AwsError::ProfileNotFound { .. }));
        assert!(settings.get().hosts.is_empty());
        assert!(!dir.path().join("settings.json").exists());
    }

    #[tokio::test]
    async fn a_failing_store_keys_saves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let settings = store_in(&dir);
        let deletes = RefCell::new(0);
        let err = add_connection_with(
            keys_conn(),
            Some(keys()),
            |_| async { Ok(ident()) },
            |_, _| Err("keychain locked".to_string()),
            |_| { *deletes.borrow_mut() += 1; Ok(()) },
            &settings,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AwsError::Other { .. }));
        assert!(settings.get().hosts.is_empty());
        assert!(!dir.path().join("settings.json").exists());
    }

    #[test]
    fn a_keys_connection_whose_keyring_ref_differs_from_its_id_is_refused() {
        let mut s = AppSettings::default();
        let mut c = keys_conn();
        c.kind = HostKind::Aws {
            auth: AwsAuth::Keys { keyring_ref: "other".into() },
            default_region: "us-west-2".into(),
        };
        s.hosts.push(c);
        assert!(aws_conn("c2", &s).is_err());
        s.hosts = vec![keys_conn()];
        assert!(aws_conn("c2", &s).is_ok());
    }

    #[test]
    fn stored_regions_and_profiles_are_revalidated() {
        let with = |profile: &str, region: &str| {
            let mut c = profile_conn();
            c.kind = HostKind::Aws {
                auth: AwsAuth::Profile { name: profile.into() },
                default_region: region.into(),
            };
            let mut s = AppSettings::default();
            s.hosts.push(c);
            s
        };
        assert!(aws_conn("c1", &with("dev", "us-west-2")).is_ok());
        assert!(aws_conn("c1", &with("dev", "not a region")).is_err());
        assert!(aws_conn("c1", &with("a b", "us-west-2")).is_err());
    }

    #[test]
    fn a_connection_with_an_invalid_stored_region_can_still_be_removed() {
        let dir = tempfile::tempdir().unwrap();
        let settings = store_in(&dir);
        let mut c = keys_conn();
        c.kind = HostKind::Aws {
            auth: AwsAuth::Keys { keyring_ref: "c2".into() },
            default_region: "not a region".into(),
        };
        settings.update(AppSettings { hosts: vec![c, profile_conn()], ..AppSettings::default() }).unwrap();
        let deleted = RefCell::new(Vec::<String>::new());
        let after = remove_connection_with(
            "c2",
            |id| { deleted.borrow_mut().push(id.to_string()); Ok(()) },
            &settings,
        )
        .unwrap();
        assert_eq!(after.hosts, vec![profile_conn()]);
        assert_eq!(settings.get().hosts, vec![profile_conn()]);
        assert_eq!(*deleted.borrow(), vec!["c2".to_string()]);
    }

    #[test]
    fn removal_leaves_the_keychain_alone_when_keyring_ref_is_not_the_id() {
        let dir = tempfile::tempdir().unwrap();
        let settings = store_in(&dir);
        let mut c = keys_conn();
        c.kind = HostKind::Aws {
            auth: AwsAuth::Keys { keyring_ref: "someone-else".into() },
            default_region: "us-west-2".into(),
        };
        settings.update(AppSettings { hosts: vec![c], ..AppSettings::default() }).unwrap();
        remove_connection_with("c2", |_| panic!("must not touch the keychain"), &settings).unwrap();
        assert!(settings.get().hosts.is_empty());
        assert!(remove_connection_with("c2", |_| Ok(()), &settings).is_err());
        assert!(remove_connection_with("bad id!", |_| Ok(()), &settings).is_err());
    }

    #[test]
    fn china_and_govcloud_regions_are_refused_for_connections() {
        for r in ["cn-north-1", "us-gov-west-1"] {
            let e = validated("Prod", r).unwrap_err();
            assert!(matches!(&e, AwsError::Other { message } if message == "China and GovCloud regions aren't supported yet."), "{e:?}");
        }
        assert!(validated("Prod", "eu-west-1").is_ok());
    }

    #[test]
    fn the_policy_document_is_every_file_of_the_tier() {
        let admin = aws_policy_document(Tier::EnvironmentAdmin);
        let names: Vec<&str> = admin.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["environment-admin.json", "environment-admin-network.json"]);
        for f in &admin {
            serde_json::from_str::<serde_json::Value>(&f.json).expect("policy JSON");
        }
        let ws = aws_policy_document(Tier::WorkspaceUser);
        assert_eq!(ws.len(), 1);
        assert_eq!(ws[0].name, "workspace-user.json");
        assert_eq!(
            serde_json::to_value(&ws[0]).unwrap()["name"],
            "workspace-user.json"
        );
    }
}
