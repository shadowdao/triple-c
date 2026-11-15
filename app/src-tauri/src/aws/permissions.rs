//! The permission check: which Triple-C tier the connection's credentials
//! can act as, by simulating each tier's checks (`policies::checks`) with
//! IAM's policy simulator, or — when the caller may not simulate itself —
//! by a few read-only probes.

use std::future::Future;

use futures_util::stream::{self, StreamExt, TryStreamExt};
use serde::Serialize;

use super::error::{classify_sdk_error, AwsError};
use super::identity::{self, principal_for_simulation, CallerIdentity, PrincipalArn};
use super::policies::{checks, render, Check, ContextValue, Placeholders, Tier};

/// How many simulations are in flight at once.
const CONCURRENCY: usize = 4;

/// One `SimulatePrincipalPolicy` request, fully rendered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimulationRequest {
    pub principal: String,
    pub action: String,
    pub resource: String,
    pub context: Vec<ContextEntry>,
}

/// A context key with every value it carries in the request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextEntry {
    pub key: String,
    pub values: Vec<String>,
}

impl ContextEntry {
    /// Sent as `StringList` when the key has more than one value, `String`
    /// otherwise.
    pub fn is_list(&self) -> bool {
        self.values.len() > 1
    }
}

/// Builds the simulation for `check`: the resource and every literal context
/// value rendered with `p`, `OwnerId` as `user_id`, and `aws:userid` =
/// `user_id` added. A key appearing more than once becomes one entry holding
/// all its values (in order, without duplicates).
pub fn simulation_request(check: &Check, p: &Placeholders, user_id: &str) -> SimulationRequest {
    let mut context: Vec<ContextEntry> = Vec::new();
    let mut add = |key: &str, value: String| match context.iter_mut().find(|e| e.key == key) {
        Some(e) if e.values.contains(&value) => {}
        Some(e) => e.values.push(value),
        None => context.push(ContextEntry {
            key: key.to_string(),
            values: vec![value],
        }),
    };
    for (key, value) in check.context {
        let value = match value {
            ContextValue::OwnerId => user_id.to_string(),
            ContextValue::Literal(template) => render(template, p),
        };
        add(key, value);
    }
    add("aws:userid", user_id.to_string());
    SimulationRequest {
        principal: p.principal.to_string(),
        action: check.action.to_string(),
        resource: render(check.resource, p),
        context,
    }
}

/// The read-only calls used when simulation isn't possible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Probe {
    EcsListClusters,
    SsmGetParametersByPath,
    CfnDescribeStacks,
    SecretsListSecrets,
}

impl Probe {
    const ALL: [Probe; 4] = [
        Probe::EcsListClusters,
        Probe::SsmGetParametersByPath,
        Probe::CfnDescribeStacks,
        Probe::SecretsListSecrets,
    ];

    /// The probe standing in for a read check's service, if there is one.
    fn for_action(action: &str) -> Option<Probe> {
        match action.split_once(':')?.0 {
            "ecs" => Some(Probe::EcsListClusters),
            "ssm" => Some(Probe::SsmGetParametersByPath),
            "cloudformation" => Some(Probe::CfnDescribeStacks),
            "secretsmanager" => Some(Probe::SecretsListSecrets),
            _ => None,
        }
    }

    fn action(self) -> &'static str {
        match self {
            Probe::EcsListClusters => "ecs:ListClusters",
            Probe::SsmGetParametersByPath => "ssm:GetParametersByPath",
            Probe::CfnDescribeStacks => "cloudformation:DescribeStacks",
            Probe::SecretsListSecrets => "secretsmanager:ListSecrets",
        }
    }
}

/// What the permission check needs from AWS. The methods return `Send`
/// futures so `check_permissions` is `Send` (a Tauri command needs that) for
/// any implementation; an impl may still write them as `async fn`.
pub trait PermissionApi: Send + Sync {
    /// The ARN to simulate for the caller's principal (`iam:GetRole` for a
    /// role session).
    fn resolve_principal(
        &self,
        principal: PrincipalArn,
    ) -> impl Future<Output = Result<String, AwsError>> + Send;
    /// Whether the simulator says `allowed`.
    fn simulate(
        &self,
        request: &SimulationRequest,
    ) -> impl Future<Output = Result<bool, AwsError>> + Send;
    fn probe(&self, probe: Probe) -> impl Future<Output = Result<(), AwsError>> + Send;
}

/// The real thing, from a connection's `SdkConfig`.
pub struct SdkPermissionApi {
    iam: aws_sdk_iam::Client,
    ecs: aws_sdk_ecs::Client,
    ssm: aws_sdk_ssm::Client,
    cfn: aws_sdk_cloudformation::Client,
    secrets: aws_sdk_secretsmanager::Client,
}

impl SdkPermissionApi {
    pub fn new(cfg: &aws_config::SdkConfig) -> Self {
        SdkPermissionApi {
            iam: aws_sdk_iam::Client::new(cfg),
            ecs: aws_sdk_ecs::Client::new(cfg),
            ssm: aws_sdk_ssm::Client::new(cfg),
            cfn: aws_sdk_cloudformation::Client::new(cfg),
            secrets: aws_sdk_secretsmanager::Client::new(cfg),
        }
    }
}

impl PermissionApi for SdkPermissionApi {
    async fn resolve_principal(&self, principal: PrincipalArn) -> Result<String, AwsError> {
        identity::resolve_principal(&self.iam, principal).await
    }

    async fn simulate(&self, request: &SimulationRequest) -> Result<bool, AwsError> {
        use aws_sdk_iam::types::{ContextEntry as SdkEntry, ContextKeyTypeEnum};
        const ACTION: &str = "iam:SimulatePrincipalPolicy";
        let entries = request.context.iter().map(|e| {
            SdkEntry::builder()
                .context_key_name(&e.key)
                .set_context_key_values(Some(e.values.clone()))
                .context_key_type(if e.is_list() {
                    ContextKeyTypeEnum::StringList
                } else {
                    ContextKeyTypeEnum::String
                })
                .build()
        });
        let out = self
            .iam
            .simulate_principal_policy()
            .policy_source_arn(&request.principal)
            .action_names(&request.action)
            .resource_arns(&request.resource)
            .set_context_entries(Some(entries.collect()))
            .send()
            .await
            .map_err(|e| classify_sdk_error(ACTION, &e))?;
        let decision = out
            .evaluation_results()
            .first()
            .map(|r| r.eval_decision().as_str())
            .ok_or_else(|| AwsError::Other {
                message: format!("{ACTION} returned no result for {}", request.action),
            })?;
        Ok(decision == "allowed")
    }

    async fn probe(&self, probe: Probe) -> Result<(), AwsError> {
        let action = probe.action();
        let classify = |e: &(dyn std::error::Error + 'static)| classify_sdk_error(action, e);
        match probe {
            Probe::EcsListClusters => self
                .ecs
                .list_clusters()
                .max_results(1)
                .send()
                .await
                .map(drop)
                .map_err(|e| classify(&e)),
            Probe::SsmGetParametersByPath => self
                .ssm
                .get_parameters_by_path()
                .path("/triple-c/environments")
                .max_results(1)
                .send()
                .await
                .map(drop)
                .map_err(|e| classify(&e)),
            Probe::CfnDescribeStacks => self
                .cfn
                .describe_stacks()
                .send()
                .await
                .map(drop)
                .map_err(|e| classify(&e)),
            Probe::SecretsListSecrets => self
                .secrets
                .list_secrets()
                .max_results(1)
                .send()
                .await
                .map(drop)
                .map_err(|e| classify(&e)),
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Allowed,
    Denied,
    Unverified,
}

/// One row per action. An action checked against several resources (or
/// with several contexts) is `Denied` if any of them is, else `Unverified`
/// if any is, else `Allowed`.
#[derive(Debug, Clone, Serialize)]
pub struct ActionResult {
    pub action: String,
    pub verdict: Verdict,
    /// Set when the row is `Unverified` because a call failed (throttling,
    /// network, ...): the error, as `AwsError` displays it. Such a row
    /// blocks its tier's claim; an `Unverified` row without a note (a write
    /// in probe mode, an unprobed read) does not.
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TierResult {
    pub tier: Tier,
    pub results: Vec<ActionResult>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    Simulated,
    /// Simulation wasn't possible; `reason` says why.
    Probed {
        reason: String,
    },
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Summary {
    EnvironmentAdmin,
    WorkspaceUser,
    Insufficient,
}

#[derive(Debug, Clone, Serialize)]
pub struct PermissionReport {
    pub identity: CallerIdentity,
    pub region: String,
    pub method: Method,
    pub summary: Summary,
    /// `[WorkspaceUser, EnvironmentAdmin]`.
    pub tiers: Vec<TierResult>,
}

const TIERS: [Tier; 2] = [Tier::WorkspaceUser, Tier::EnvironmentAdmin];

/// One check's result, before checks are folded into per-action rows.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Outcome {
    Allowed,
    Denied,
    /// Not checkable by design (a write in probe mode, an unprobed read).
    Unverified,
    /// The call failed for a reason that says nothing about the permission.
    Failed(String),
}

/// The worst of `outcomes`: Denied, then Failed (as `Unverified` with the
/// first failure as its note), then Unverified, then Allowed.
fn aggregate(outcomes: &[Outcome]) -> (Verdict, Option<String>) {
    if outcomes.contains(&Outcome::Denied) {
        return (Verdict::Denied, None);
    }
    if let Some(note) = outcomes.iter().find_map(|o| match o {
        Outcome::Failed(m) => Some(m.clone()),
        _ => None,
    }) {
        return (Verdict::Unverified, Some(note));
    }
    if outcomes.contains(&Outcome::Unverified) {
        return (Verdict::Unverified, None);
    }
    (Verdict::Allowed, None)
}

/// Folds per-check outcomes (in `checks(tier)` order) into one row per
/// action, in first-appearance order.
fn tier_result(tier: Tier, outcomes: Vec<Outcome>) -> TierResult {
    let mut rows: Vec<(&'static str, Vec<Outcome>)> = Vec::new();
    for (check, outcome) in checks(tier).iter().zip(outcomes) {
        match rows.iter_mut().find(|(a, _)| *a == check.action) {
            Some((_, os)) => os.push(outcome),
            None => rows.push((check.action, vec![outcome])),
        }
    }
    TierResult {
        tier,
        results: rows
            .into_iter()
            .map(|(action, os)| {
                let (verdict, note) = aggregate(&os);
                ActionResult {
                    action: action.to_string(),
                    verdict,
                    note,
                }
            })
            .collect(),
    }
}

/// A tier is claimed when none of its rows is `Denied` or failed (has a
/// note). Design-level `Unverified` rows don't block. A probed run never
/// claims the admin tier (R15): it is nearly all writes, which probes can't
/// see, so "nothing denied" would mean nothing.
fn summary(tiers: &[TierResult], probed: bool) -> Summary {
    let clear = |t: Tier| {
        tiers.iter().filter(|r| r.tier == t).all(|r| {
            r.results
                .iter()
                .all(|a| a.verdict != Verdict::Denied && a.note.is_none())
        })
    };
    if !clear(Tier::WorkspaceUser) {
        Summary::Insufficient
    } else if !probed && clear(Tier::EnvironmentAdmin) {
        Summary::EnvironmentAdmin
    } else {
        Summary::WorkspaceUser
    }
}

/// Errors that mean the credentials themselves don't work: the UI acts on
/// these (sign in, re-enter keys) rather than showing a report.
fn is_credential_error(e: &AwsError) -> bool {
    matches!(
        e,
        AwsError::SsoExpired { .. }
            | AwsError::InvalidCredentials { .. }
            | AwsError::ProfileNotFound { .. }
            | AwsError::KeysMissing
    )
}

/// Which tiers the caller's credentials can act as, in `region`.
///
/// Simulates every check of both tiers against the caller's principal. It
/// falls back to probes when the principal can't be simulated (root,
/// federated user), resolving it is denied (`iam:GetRole`), or the first
/// simulation is denied. In probe mode a read check whose service has a
/// probe takes that probe's result; the check behind the fallback (the
/// caller's own `iam:GetRole` / `iam:SimulatePrincipalPolicy`) is `Denied`;
/// every other check is `Unverified`.
///
/// A credential error (SSO expired, invalid or missing credentials) from
/// any call is returned as `Err`, as is any other failure resolving the
/// principal. Other failures of individual checks (throttling, network)
/// make that row `Unverified` with a note, which blocks its tier's claim.
/// See [`summary`] for the claim rules.
pub async fn check_permissions<A: PermissionApi>(
    api: &A,
    identity: &CallerIdentity,
    region: &str,
) -> Result<PermissionReport, AwsError> {
    let (method, outcomes) = match simulate_all(api, identity, region).await? {
        Simulation::Done(outcomes) => (Method::Simulated, outcomes),
        Simulation::Fallback { reason, denied } => (
            Method::Probed { reason },
            probe_all(api, denied.as_deref()).await?,
        ),
    };
    let failed: Vec<&str> = outcomes
        .iter()
        .flatten()
        .filter_map(|o| match o {
            Outcome::Failed(m) => Some(m.as_str()),
            _ => None,
        })
        .collect();
    if let Some(first) = failed.first() {
        let total: usize = outcomes.iter().map(Vec::len).sum();
        log::warn!(
            "AWS permission check: {} of {total} checks could not be evaluated (first: {first})",
            failed.len()
        );
    }
    let probed = matches!(method, Method::Probed { .. });
    let tiers: Vec<TierResult> = TIERS
        .into_iter()
        .zip(outcomes)
        .map(|(tier, os)| tier_result(tier, os))
        .collect();
    Ok(PermissionReport {
        identity: identity.clone(),
        region: region.to_string(),
        method,
        summary: summary(&tiers, probed),
        tiers,
    })
}

enum Simulation {
    Done([Vec<Outcome>; 2]),
    /// Simulation isn't possible. `denied` is the action whose denial
    /// caused it, if one did.
    Fallback {
        reason: String,
        denied: Option<String>,
    },
}

impl Simulation {
    fn fallback(e: AwsError) -> Self {
        let denied = match &e {
            AwsError::AccessDenied { action, .. } => Some(action.clone()),
            _ => None,
        };
        Simulation::Fallback {
            reason: e.to_string(),
            denied,
        }
    }
}

fn simulated_outcome(r: Result<bool, AwsError>) -> Result<Outcome, AwsError> {
    match r {
        Ok(true) => Ok(Outcome::Allowed),
        Ok(false) => Ok(Outcome::Denied),
        Err(e) if is_credential_error(&e) => Err(e),
        Err(e) => Ok(Outcome::Failed(e.to_string())),
    }
}

/// Per-tier outcomes by simulation, or why simulation isn't possible.
async fn simulate_all<A: PermissionApi>(
    api: &A,
    identity: &CallerIdentity,
    region: &str,
) -> Result<Simulation, AwsError> {
    let principal = match principal_for_simulation(&identity.arn) {
        Ok(p) => p,
        Err(e) => return Ok(Simulation::fallback(e)),
    };
    let principal = match api.resolve_principal(principal).await {
        Ok(p) => p,
        Err(e @ AwsError::AccessDenied { .. }) => return Ok(Simulation::fallback(e)),
        Err(e) => return Err(e),
    };
    let p = Placeholders {
        region,
        account: &identity.account,
        principal: &principal,
    };
    let mut requests = TIERS
        .into_iter()
        .flat_map(|t| checks(t).iter())
        .map(|c| simulation_request(c, &p, &identity.user_id))
        .collect::<Vec<_>>()
        .into_iter();
    let Some(first) = requests.next() else {
        return Ok(Simulation::Done([Vec::new(), Vec::new()]));
    };
    // The first simulation alone: if the caller may not simulate at all,
    // that's the moment to fall back, before spending the rest.
    let first = match api.simulate(&first).await {
        Err(e @ AwsError::AccessDenied { .. }) => return Ok(Simulation::fallback(e)),
        r => simulated_outcome(r)?,
    };
    // Owned requests: a closure over borrowed ones makes the future's
    // `Send` unprovable (rustc's higher-ranked closure limitation).
    let rest: Vec<Outcome> = stream::iter(requests)
        .map(|r| async move { simulated_outcome(api.simulate(&r).await) })
        .buffered(CONCURRENCY)
        .try_collect()
        .await?;
    let mut all = std::iter::once(first).chain(rest);
    Ok(Simulation::Done(
        TIERS.map(|t| all.by_ref().take(checks(t).len()).collect()),
    ))
}

/// Per-tier outcomes from the four probes. `denied` is the action whose
/// denial forced probing; the caller's own-principal checks of it are
/// `Denied`.
async fn probe_all<A: PermissionApi>(
    api: &A,
    denied: Option<&str>,
) -> Result<[Vec<Outcome>; 2], AwsError> {
    let results =
        futures_util::future::join_all(Probe::ALL.map(|p| async move { (p, api.probe(p).await) }))
            .await;
    if let Some((_, Err(e))) = results
        .iter()
        .find(|(_, r)| matches!(r, Err(e) if is_credential_error(e)))
    {
        return Err(e.clone());
    }
    let probe_outcome = |probe: Probe| match results.iter().find(|(p, _)| *p == probe) {
        Some((_, Ok(()))) => Outcome::Allowed,
        Some((_, Err(AwsError::AccessDenied { .. }))) => Outcome::Denied,
        Some((_, Err(e))) => Outcome::Failed(e.to_string()),
        None => Outcome::Unverified,
    };
    Ok(TIERS.map(|t| {
        checks(t)
            .iter()
            .map(|c| {
                if denied == Some(c.action) && c.resource == "{principal}" {
                    return Outcome::Denied;
                }
                match Probe::for_action(c.action) {
                    Some(probe) if !c.write => probe_outcome(probe),
                    _ => Outcome::Unverified,
                }
            })
            .collect()
    }))
}

#[cfg(test)]
mod tests;
