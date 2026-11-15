//! Commands for host connections (AWS): add, update, remove, identity,
//! sign-in and the permission check.

use std::future::Future;

use tauri::State;

use crate::aws::permissions::{check_permissions, PermissionReport, SdkPermissionApi};
use crate::aws::policies::{policy_json, Tier};
use crate::aws::{caller_identity, sdk_config, sdk_config_for_keys, AwsError, CallerIdentity};
use crate::models::{
    validate_connection_name, validate_profile_name, validate_region, AppSettings, AwsAuth,
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

    let mut all = settings.get();
    all.hosts.push(conn.clone());
    if let Some(k) = &keys {
        store_keys(&conn.id, k).map_err(other)?;
    }
    if let Err(e) = settings.update(all) {
        if keys.is_some() {
            let _ = delete_keys(&conn.id);
        }
        return Err(other(e));
    }
    Ok(conn)
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

fn aws_conn(id: &str, settings: &AppSettings) -> Result<HostConnection, String> {
    check_id(id)?;
    settings
        .hosts
        .iter()
        .find(|h| h.id == id)
        .cloned()
        .ok_or_else(|| "Connection not found".to_string())
}

fn validated(name: &str, region: &str) -> Result<(String, String), AwsError> {
    Ok((
        validate_connection_name(name).map_err(other)?,
        validate_region(region).map_err(other)?,
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
    let mut settings = state.settings_store.get();
    aws_conn(&id, &settings).map_err(other)?;
    let conn = settings.hosts.iter_mut().find(|h| h.id == id).expect("checked above");
    conn.name = name;
    let HostKind::Aws { default_region, .. } = &mut conn.kind;
    *default_region = region;
    let updated = conn.clone();
    state.settings_store.update(settings).map_err(other)?;
    Ok(updated)
}

#[tauri::command]
pub async fn remove_host_connection(
    id: String,
    state: State<'_, AppState>,
) -> Result<AppSettings, String> {
    let mut settings = state.settings_store.get();
    let conn = aws_conn(&id, &settings)?;
    let HostKind::Aws { auth, .. } = &conn.kind;
    if let AwsAuth::Keys { keyring_ref } = auth {
        secure::delete_aws_keys(keyring_ref)?;
    }
    settings.hosts.retain(|h| h.id != id);
    state.settings_store.update(settings)
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
    validate_profile_name(name)?;
    super::aws_commands::run_sso_login(name).await
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
        Some(r) => validate_region(&r).map_err(other)?,
        None => default_region.clone(),
    };
    let cfg = sdk_config(&conn, &region).await?;
    let identity = caller_identity(&cfg).await?;
    let api = SdkPermissionApi::new(&cfg);
    check_permissions(&api, &identity, &region).await
}

#[tauri::command]
pub fn aws_policy_document(tier: Tier) -> String {
    policy_json(tier).to_string()
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
    }
}
