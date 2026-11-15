//! Who the connection's credentials are, and which IAM principal the
//! policy simulator should be asked about.

use aws_config::SdkConfig;
use serde::Serialize;

use super::error::{classify_sdk_error, AwsError};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CallerIdentity {
    pub account: String,
    pub arn: String,
    /// STS `UserId`: the principal's unique id (for a role session,
    /// `<role id>:<session name>`).
    pub user_id: String,
}

/// `sts:GetCallerIdentity`, which needs no IAM permission: it is the
/// connection test itself.
#[allow(dead_code)] // Called by the Tauri commands (Task 6).
pub async fn caller_identity(cfg: &SdkConfig) -> Result<CallerIdentity, AwsError> {
    const ACTION: &str = "sts:GetCallerIdentity";
    let out = aws_sdk_sts::Client::new(cfg)
        .get_caller_identity()
        .send()
        .await
        .map_err(|e| classify_sdk_error(ACTION, &e))?;
    let field = |v: Option<&str>, name: &str| {
        v.map(str::to_string).ok_or_else(|| AwsError::Other {
            message: format!("{ACTION} returned no {name}"),
        })
    };
    Ok(CallerIdentity {
        account: field(out.account(), "Account")?,
        arn: field(out.arn(), "Arn")?,
        user_id: field(out.user_id(), "UserId")?,
    })
}

/// The principal to simulate, as far as the caller's ARN tells us.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrincipalArn {
    /// An IAM user or role ARN, usable as `PolicySourceArn` directly.
    Exact(String),
    /// A role session: the role's ARN carries a path the session ARN lacks,
    /// so it has to be looked up (`resolve_principal`).
    AssumedRole { account: String, role_name: String },
}

/// Map the caller's ARN to a simulatable principal. Root, federated users
/// and anything unrecognised cannot be simulated; the caller falls back to
/// probing instead.
pub fn principal_for_simulation(caller_arn: &str) -> Result<PrincipalArn, AwsError> {
    let unsupported = || AwsError::Other {
        message: format!("IAM policy simulation isn't possible for the principal {caller_arn}"),
    };
    // arn:<partition>:<service>:<region>:<account>:<resource>
    let mut parts = caller_arn.splitn(6, ':');
    let (Some("arn"), Some(partition), Some(service), Some(_region), Some(account), Some(resource)) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return Err(unsupported());
    };
    if partition.is_empty() || account.is_empty() {
        return Err(unsupported());
    }
    match service {
        "iam" if resource.starts_with("user/") || resource.starts_with("role/") => {
            Ok(PrincipalArn::Exact(caller_arn.to_string()))
        }
        "sts" => match resource
            .strip_prefix("assumed-role/")
            .and_then(|r| r.split_once('/'))
        {
            Some((role_name, _session)) if !role_name.is_empty() => Ok(PrincipalArn::AssumedRole {
                account: account.to_string(),
                role_name: role_name.to_string(),
            }),
            _ => Err(unsupported()),
        },
        _ => Err(unsupported()),
    }
}

/// The full ARN to simulate. An assumed role needs `iam:GetRole` to recover
/// the role's path (SSO roles live under `/aws-reserved/...`).
pub async fn resolve_principal(
    iam: &aws_sdk_iam::Client,
    p: PrincipalArn,
) -> Result<String, AwsError> {
    const ACTION: &str = "iam:GetRole";
    let (account, role_name) = match p {
        PrincipalArn::Exact(arn) => return Ok(arn),
        PrincipalArn::AssumedRole { account, role_name } => (account, role_name),
    };
    let out = iam
        .get_role()
        .role_name(&role_name)
        .send()
        .await
        .map_err(|e| classify_sdk_error(ACTION, &e))?;
    let arn = out
        .role()
        .map(|r| r.arn().to_string())
        .filter(|a| !a.is_empty())
        .ok_or_else(|| AwsError::Other {
            message: format!("{ACTION} returned no role for {role_name}"),
        })?;
    // The role must be the session's own: same account.
    if arn.split(':').nth(4) != Some(account.as_str()) {
        return Err(AwsError::Other {
            message: format!("{ACTION} for {role_name} returned {arn}, outside account {account}"),
        });
    }
    Ok(arn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aws::{classify_sdk_error, AwsError};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn iam_user_arn_is_used_as_is() {
        let arn = "arn:aws:iam::111122223333:user/alice";
        assert_eq!(
            principal_for_simulation(arn).unwrap(),
            PrincipalArn::Exact(arn.into())
        );
    }

    #[test]
    fn iam_role_arn_is_used_as_is() {
        let arn = "arn:aws:iam::111122223333:role/ops/deployer";
        assert_eq!(
            principal_for_simulation(arn).unwrap(),
            PrincipalArn::Exact(arn.into())
        );
    }

    #[test]
    fn assumed_role_arn_maps_to_role_name() {
        let arn = "arn:aws:sts::111122223333:assumed-role/AWSReservedSSO_Admin_abc/alice@x.com";
        assert_eq!(
            principal_for_simulation(arn).unwrap(),
            PrincipalArn::AssumedRole {
                account: "111122223333".into(),
                role_name: "AWSReservedSSO_Admin_abc".into(),
            }
        );
    }

    #[test]
    fn root_and_federated_arns_are_rejected_for_simulation() {
        for arn in [
            "arn:aws:iam::1:root",
            "arn:aws:sts::1:federated-user/bob",
            "not-an-arn",
        ] {
            match principal_for_simulation(arn) {
                Err(AwsError::Other { message }) => {
                    assert!(
                        message.contains("simulation isn't possible"),
                        "{arn}: {message}"
                    )
                }
                other => panic!("{arn}: expected Other, got {other:?}"),
            }
        }
    }

    /// A one-shot-per-connection HTTP server that answers every request with
    /// `status` and `body`, and reports each request it saw.
    async fn fake_endpoint(
        status: &'static str,
        body: &'static str,
    ) -> (String, tokio::sync::mpsc::UnboundedReceiver<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let tx = tx.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 64 * 1024];
                    let mut req = Vec::new();
                    loop {
                        let n = match sock.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => n,
                        };
                        req.extend_from_slice(&buf[..n]);
                        let text = String::from_utf8_lossy(&req).into_owned();
                        let Some(head_end) = text.find("\r\n\r\n") else {
                            continue;
                        };
                        let len = text[..head_end]
                            .lines()
                            .find_map(|l| {
                                let (k, v) = l.split_once(':')?;
                                k.eq_ignore_ascii_case("content-length")
                                    .then(|| v.trim().parse::<usize>().ok())
                                    .flatten()
                            })
                            .unwrap_or(0);
                        if req.len() < head_end + 4 + len {
                            continue;
                        }
                        let _ = tx.send(text);
                        let resp = format!(
                            "HTTP/1.1 {status}\r\nContent-Type: text/xml\r\nContent-Length: {}\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = sock.write_all(resp.as_bytes()).await;
                        req.clear();
                    }
                });
            }
        });
        (format!("http://127.0.0.1:{port}"), rx)
    }

    fn iam_client(endpoint: String) -> aws_sdk_iam::Client {
        let conf = aws_sdk_iam::Config::builder()
            .behavior_version(aws_sdk_iam::config::BehaviorVersion::latest())
            .region(aws_sdk_iam::config::Region::new("us-east-1"))
            .credentials_provider(aws_sdk_iam::config::Credentials::new(
                "AKIDTEST", "secret", None, None, "test",
            ))
            .endpoint_url(endpoint)
            .build();
        aws_sdk_iam::Client::from_conf(conf)
    }

    const ROLE_ARN: &str = "arn:aws:iam::111122223333:role/aws-reserved/sso.amazonaws.com/us-west-2/AWSReservedSSO_Admin_abc";

    #[tokio::test]
    async fn assumed_role_arn_maps_to_role_arn_with_path() {
        const BODY: &str = r#"<GetRoleResponse xmlns="https://iam.amazonaws.com/doc/2010-05-08/">
  <GetRoleResult>
    <Role>
      <Path>/aws-reserved/sso.amazonaws.com/us-west-2/</Path>
      <RoleName>AWSReservedSSO_Admin_abc</RoleName>
      <RoleId>AROAEXAMPLEID</RoleId>
      <Arn>arn:aws:iam::111122223333:role/aws-reserved/sso.amazonaws.com/us-west-2/AWSReservedSSO_Admin_abc</Arn>
      <CreateDate>2024-01-01T00:00:00Z</CreateDate>
    </Role>
  </GetRoleResult>
  <ResponseMetadata><RequestId>req-1</RequestId></ResponseMetadata>
</GetRoleResponse>"#;
        let (endpoint, mut seen) = fake_endpoint("200 OK", BODY).await;
        let p = PrincipalArn::AssumedRole {
            account: "111122223333".into(),
            role_name: "AWSReservedSSO_Admin_abc".into(),
        };
        let arn = resolve_principal(&iam_client(endpoint), p).await.unwrap();
        assert_eq!(arn, ROLE_ARN);
        let req = seen.recv().await.unwrap();
        assert!(req.contains("Action=GetRole"), "{req}");
        assert!(req.contains("RoleName=AWSReservedSSO_Admin_abc"), "{req}");
    }

    #[tokio::test]
    async fn an_exact_principal_needs_no_lookup() {
        // No endpoint answers: any request would fail the test.
        let client = iam_client("http://127.0.0.1:9".into());
        let arn = "arn:aws:iam::111122223333:user/alice";
        let got = resolve_principal(&client, PrincipalArn::Exact(arn.into()))
            .await
            .unwrap();
        assert_eq!(got, arn);
    }

    #[tokio::test]
    async fn a_denied_get_role_is_access_denied_for_get_role() {
        const BODY: &str = r#"<ErrorResponse xmlns="https://iam.amazonaws.com/doc/2010-05-08/">
  <Error>
    <Type>Sender</Type>
    <Code>AccessDenied</Code>
    <Message>User: arn:aws:sts::111122223333:assumed-role/R/alice is not authorized to perform: iam:GetRole</Message>
  </Error>
  <RequestId>req-2</RequestId>
</ErrorResponse>"#;
        let (endpoint, _seen) = fake_endpoint("403 Forbidden", BODY).await;
        let p = PrincipalArn::AssumedRole {
            account: "111122223333".into(),
            role_name: "R".into(),
        };
        match resolve_principal(&iam_client(endpoint), p).await {
            Err(AwsError::AccessDenied { action, message }) => {
                assert_eq!(action, "iam:GetRole");
                assert!(
                    message.contains("is not authorized to perform: iam:GetRole"),
                    "{message}"
                );
            }
            other => panic!("expected AccessDenied, got {other:?}"),
        }
    }

    #[derive(Debug)]
    struct Msg(&'static str, Option<Box<dyn std::error::Error + 'static>>);
    impl std::fmt::Display for Msg {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(self.0)
        }
    }
    impl std::error::Error for Msg {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            self.1.as_deref()
        }
    }

    #[test]
    fn an_expired_sso_token_is_reported_as_sso_expired() {
        for leaf in [
            "The SSO session associated with this profile has expired or is otherwise invalid",
            "Token has expired and refresh failed",
        ] {
            // Deep in the chain, as the SDK reports it.
            let err = Msg("dispatch failure", Some(Box::new(Msg(leaf, None))));
            assert!(
                matches!(
                    classify_sdk_error("sts:GetCallerIdentity", &err),
                    AwsError::SsoExpired { .. }
                ),
                "{leaf}"
            );
        }
    }

    /// SSO's GetRoleCredentials on a revoked or signed-out session. Inside a
    /// profile connection's credentials that means "sign in again".
    #[test]
    fn a_revoked_sso_session_is_reported_as_sso_expired() {
        use crate::aws::error::ProfileCredentialsError;
        use aws_credential_types::provider::error::CredentialsError;
        let leaf = CredentialsError::provider_error(
            "service error: unhandled error (UnauthorizedException): Session token not found or invalid",
        );
        let err = CredentialsError::provider_error(ProfileCredentialsError {
            profile: "dev".into(),
            not_loaded: false,
            source: leaf,
        });
        assert_eq!(
            classify_sdk_error(
                "sts:GetCallerIdentity",
                &Msg("dispatch failure", Some(Box::new(err)))
            ),
            AwsError::SsoExpired {
                profile: "dev".into()
            }
        );
    }

    /// The same error code from some other service, outside any profile's
    /// credentials, is not an SSO problem.
    #[test]
    fn an_unauthorized_exception_outside_profile_credentials_is_not_sso() {
        let err = Msg(
            "service error: unhandled error (UnauthorizedException): Session token not found or invalid",
            None,
        );
        assert!(!matches!(
            classify_sdk_error("ecs:ListClusters", &err),
            AwsError::SsoExpired { .. }
        ));
    }

    #[test]
    fn service_error_codes_are_classified() {
        let denied = Msg(
            "service error: unhandled error (AccessDeniedException)",
            None,
        );
        assert!(matches!(
            classify_sdk_error("ecs:ListClusters", &denied),
            AwsError::AccessDenied { action, .. } if action == "ecs:ListClusters"
        ));
        for code in [
            "ExpiredToken",
            "InvalidClientTokenId",
            "SignatureDoesNotMatch",
        ] {
            let text: &'static str =
                Box::leak(format!("service error: unhandled error ({code})").into_boxed_str());
            assert!(
                matches!(
                    classify_sdk_error("sts:GetCallerIdentity", &Msg(text, None)),
                    AwsError::InvalidCredentials { .. }
                ),
                "{code}"
            );
        }
        assert!(matches!(
            classify_sdk_error(
                "sts:GetCallerIdentity",
                &Msg("io error: connection refused", None)
            ),
            AwsError::Other { .. }
        ));
    }
}
