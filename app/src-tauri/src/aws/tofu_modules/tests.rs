use std::collections::BTreeSet;

use hcl::expr::{Expression, ObjectKey};
use hcl::{Block, Body};
use serde_json::json;

use super::*;
use crate::aws::policies::PERMISSIONS_BOUNDARY_NAME;

const RECORD_FIELDS: [&str; 11] = [
    "schema",
    "name",
    "template_version",
    "cluster_arn",
    "security_group_id",
    "subnet_ids",
    "assign_public_ip",
    "kms_key_arn",
    "log_group",
    "network_mode",
    "tenancy",
];

const VARIABLES: [&str; 7] = [
    "environment_name",
    "region",
    "network_mode",
    "vpc_id",
    "subnet_ids",
    "assign_public_ip",
    "log_retention_days",
];

/// Every `.tf` file of `kind`, parsed and merged into one body.
fn parsed(kind: ModuleKind) -> Body {
    let mut structures = Vec::new();
    for (name, text) in module_files(kind) {
        if name.ends_with(".tf") {
            let body = hcl::parse(text).unwrap_or_else(|e| panic!("{name}: {e}"));
            structures.extend(body.into_inner());
        }
    }
    assert!(!structures.is_empty(), "{kind:?} has no .tf files");
    Body::from(structures)
}

fn tf_text(kind: ModuleKind) -> String {
    module_files(kind)
        .iter()
        .filter(|(name, _)| name.ends_with(".tf"))
        .map(|(_, text)| *text)
        .collect::<Vec<_>>()
        .join("\n")
}

fn label(block: &Block, i: usize) -> Option<&str> {
    block.labels.get(i).map(|l| l.as_str())
}

/// Every block in `body`, at any depth.
fn all_blocks(body: &Body) -> Vec<&Block> {
    let mut out = Vec::new();
    for b in body.blocks() {
        out.push(b);
        out.extend(all_blocks(&b.body));
    }
    out
}

fn top_blocks<'a>(body: &'a Body, ident: &'a str) -> impl Iterator<Item = &'a Block> + 'a {
    body.blocks()
        .filter(move |b| b.identifier.as_str() == ident)
}

fn resources<'a>(body: &'a Body, ty: &'a str) -> Vec<&'a Block> {
    top_blocks(body, "resource")
        .filter(|b| label(b, 0) == Some(ty))
        .collect()
}

fn attr<'a>(body: &'a Body, key: &str) -> Option<&'a Expression> {
    body.attributes()
        .find(|a| a.key.as_str() == key)
        .map(|a| &a.expr)
}

fn key_name(key: &ObjectKey) -> String {
    match key {
        ObjectKey::Identifier(id) => id.as_str().to_string(),
        ObjectKey::Expression(Expression::String(s)) => s.clone(),
        other => panic!("unexpected object key {other:?}"),
    }
}

fn object_keys(expr: &Expression) -> BTreeSet<String> {
    match expr {
        Expression::Object(obj) => obj.keys().map(key_name).collect(),
        other => panic!("expected an object, got {other:?}"),
    }
}

fn object_get<'a>(expr: &'a Expression, key: &str) -> Option<&'a Expression> {
    match expr {
        Expression::Object(obj) => obj.iter().find(|(k, _)| key_name(k) == key).map(|(_, v)| v),
        other => panic!("expected an object, got {other:?}"),
    }
}

fn render(expr: &Expression) -> String {
    hcl::format::to_string(expr).unwrap()
}

fn aws_provider(body: &Body) -> &Block {
    let providers: Vec<_> = top_blocks(body, "provider").collect();
    assert_eq!(providers.len(), 1, "exactly one provider block");
    assert_eq!(label(providers[0], 0), Some("aws"));
    providers[0]
}

fn default_tags(body: &Body) -> &Expression {
    let provider = aws_provider(body);
    let tags_blocks: Vec<_> = top_blocks(&provider.body, "default_tags").collect();
    assert_eq!(tags_blocks.len(), 1, "one default_tags block");
    attr(&tags_blocks[0].body, "tags").expect("default_tags.tags")
}

/// The environment record: the `jsonencode` argument of the SSM parameter's value.
fn record(body: &Body) -> (&Block, &Expression) {
    let params = resources(body, "aws_ssm_parameter");
    assert_eq!(params.len(), 1, "one SSM parameter");
    let value = attr(&params[0].body, "value").expect("parameter value");
    let Expression::FuncCall(call) = value else {
        panic!("the record is not jsonencode(...): {value:?}")
    };
    assert_eq!(call.name.name.as_str(), "jsonencode");
    assert_eq!(call.args.len(), 1);
    (params[0], &call.args[0])
}

#[test]
fn environment_names_are_validated() {
    for ok in ["dev", "team-a1", "a", "abcdefghij0123456789"] {
        assert_eq!(validate_environment_name(ok), Ok(ok.to_string()), "{ok}");
    }
    for bad in [
        "",
        "Dev",
        "1dev",
        "-dev",
        "dev-",
        "a--b",
        "abcdefghij0123456789x",
        "dev/x",
        "dev_x",
        "dév",
        "dev ",
    ] {
        assert!(validate_environment_name(bad).is_err(), "{bad:?} accepted");
    }
    assert_eq!(
        environment_parameter_name("dev"),
        "/triple-c/environments/dev"
    );
}

#[test]
fn owner_keys_are_stable_and_role_safe() {
    let a = owner_key("AROAEXAMPLEID:alice@example.com");
    assert_eq!(a.len(), 12);
    assert!(
        a.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')),
        "{a}"
    );
    assert_eq!(a, owner_key("AROAEXAMPLEID:alice@example.com"));
    assert_ne!(a, owner_key("AROAEXAMPLEID:bob@example.com"));
    // The key is the first 12 hex characters of the SHA-256 of the user ID.
    assert_eq!(owner_key("AIDAEXAMPLE"), sha256_prefix("AIDAEXAMPLE"));

    let env = "abcdefghij0123456789";
    assert_eq!(env.len(), 20);
    let (task, exec) = user_role_names(env, &a);
    assert_eq!(task, format!("triple-c-{env}-{a}-task"));
    assert_eq!(exec, format!("triple-c-{env}-{a}-exec"));
    assert!(task.len() <= 64 && exec.len() <= 64, "{task} / {exec}");
    for name in [&task, &exec] {
        assert!(
            name.bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"+=,.@_-".contains(&c)),
            "{name} is not a valid role name"
        );
    }
}

fn sha256_prefix(s: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(s.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()[..12]
        .to_string()
}

#[test]
fn state_bucket_and_keys_follow_d9() {
    assert_eq!(BOUNDARY_POLICY_NAME, PERMISSIONS_BOUNDARY_NAME);
    assert_eq!(BOUNDARY_POLICY_NAME, "triple-c-permissions-boundary");

    assert_eq!(
        state_bucket_name("111122223333", "us-west-2"),
        "triple-c-tfstate-111122223333-us-west-2"
    );
    // The longest commercial region name still fits S3's 63 characters.
    let longest = state_bucket_name("111122223333", "ap-southeast-5");
    assert!(longest.len() <= 63, "{longest}");

    assert_eq!(
        state_key(Deployment::Environment("dev")),
        "environments/dev/terraform.tfstate"
    );
    assert_eq!(
        state_key(Deployment::User {
            env: "dev",
            owner_key: "0123456789ab"
        }),
        "users/dev/0123456789ab/terraform.tfstate"
    );
    assert_eq!(
        state_key(Deployment::Boundary),
        "boundary/terraform.tfstate"
    );
}

#[test]
fn subnet_availability_zones_are_stable_once_chosen() {
    let body = parsed(ModuleKind::Environment);
    // Not filtered on `state`: an impaired AZ must not reorder the list.
    let azs: Vec<_> = top_blocks(&body, "data")
        .filter(|b| label(b, 0) == Some("aws_availability_zones"))
        .collect();
    assert_eq!(azs.len(), 1);
    assert!(
        attr(&azs[0].body, "state").is_none(),
        "aws_availability_zones must not filter on state"
    );
    // And each subnet ignores later AZ changes, so a moved list never
    // replaces a subnet.
    let subnets = resources(&body, "aws_subnet");
    assert_eq!(subnets.len(), 2, "public and private subnets");
    for subnet in subnets {
        let lifecycle = top_blocks(&subnet.body, "lifecycle")
            .next()
            .unwrap_or_else(|| panic!("{:?} has no lifecycle block", subnet.labels));
        let ignored = attr(&lifecycle.body, "ignore_changes").expect("ignore_changes");
        assert!(
            render(ignored).contains("availability_zone"),
            "{:?} does not ignore availability_zone changes",
            subnet.labels
        );
    }
}

#[test]
fn the_module_versions_agree() {
    assert_eq!(ENVIRONMENT_MODULE_VERSION, 1);
    assert_eq!(ONBOARD_MODULE_VERSION, 1);
    let body = parsed(ModuleKind::Environment);
    let locals: Vec<_> = top_blocks(&body, "locals")
        .filter_map(|b| attr(&b.body, "template_version"))
        .collect();
    assert_eq!(locals.len(), 1, "one locals.template_version");
    assert_eq!(
        locals[0],
        &Expression::Number(ENVIRONMENT_MODULE_VERSION.into())
    );
    let tag = object_get(default_tags(&body), "triple-c:template-version")
        .expect("template-version default tag");
    assert!(
        render(tag).contains("local.template_version"),
        "template-version tag is {}",
        render(tag)
    );
    let (_, rec) = record(&body);
    assert!(render(object_get(rec, "template_version").unwrap()).contains("local.template_version"));
}

#[test]
fn default_tags_mark_the_environment_and_nothing_overrides_component() {
    let body = parsed(ModuleKind::Environment);
    let tags = default_tags(&body);
    assert_eq!(
        object_keys(tags),
        BTreeSet::from([
            "triple-c:component".to_string(),
            "triple-c:environment".to_string(),
            "triple-c:template-version".to_string(),
        ])
    );
    assert_eq!(
        object_get(tags, "triple-c:component"),
        Some(&Expression::String("environment".into()))
    );
    assert_eq!(
        render(object_get(tags, "triple-c:environment").unwrap()),
        "var.environment_name"
    );

    // The provider's default_tags is the only place the component is named,
    // and no resource sets tags that could shadow it.
    assert_eq!(
        tf_text(ModuleKind::Environment)
            .matches("triple-c:component")
            .count(),
        1
    );
    for block in all_blocks(&body) {
        if block.identifier.as_str() == "default_tags" {
            continue;
        }
        for key in ["tags", "tags_all"] {
            if let Some(t) = attr(&block.body, key) {
                let keys = object_keys(t);
                assert!(
                    keys.iter().all(|k| !k.starts_with("triple-c:")),
                    "{:?} sets triple-c tags {keys:?}",
                    block.labels
                );
            }
        }
    }
}

#[test]
fn the_security_group_has_no_ingress() {
    let body = parsed(ModuleKind::Environment);
    for ty in [
        "aws_vpc_security_group_ingress_rule",
        "aws_security_group_rule",
    ] {
        assert!(resources(&body, ty).is_empty(), "{ty} present");
    }
    assert!(
        all_blocks(&body)
            .iter()
            .all(|b| b.identifier.as_str() != "ingress"),
        "an ingress block is present"
    );
    let groups = resources(&body, "aws_security_group");
    assert_eq!(groups.len(), 1);
    let sg = groups[0];
    assert_eq!(
        render(attr(&sg.body, "name").unwrap()),
        "\"triple-c-${var.environment_name}-tasks\""
    );
    // `ingress = []` makes OpenTofu remove any rule added out of band.
    assert_eq!(attr(&sg.body, "ingress"), Some(&Expression::Array(vec![])));
    let egress: Vec<_> = top_blocks(&sg.body, "egress").collect();
    assert_eq!(egress.len(), 1, "one egress block");
    assert_eq!(
        attr(&egress[0].body, "protocol"),
        Some(&Expression::String("-1".into()))
    );
}

const ALL_MODULES: [ModuleKind; 3] = [
    ModuleKind::Environment,
    ModuleKind::UserOnboard,
    ModuleKind::PermissionsBoundary,
];

#[test]
fn no_provider_endpoint_overrides() {
    for kind in ALL_MODULES {
        no_provider_endpoint_overrides_in(kind);
    }
}

fn no_provider_endpoint_overrides_in(kind: ModuleKind) {
    let body = parsed(kind);
    let provider = aws_provider(&body);
    assert_eq!(
        render(attr(&provider.body, "region").unwrap()),
        "var.region"
    );
    assert!(attr(&provider.body, "endpoints").is_none());
    assert!(top_blocks(&provider.body, "endpoints").next().is_none());
    // A role's `assume_role_policy` is not a provider `assume_role`.
    let text = tf_text(kind).replace("assume_role_policy", "");
    for needle in [
        "endpoints",
        "endpoint_url",
        "skip_credentials_validation",
        "assume_role",
    ] {
        assert!(!text.contains(needle), "the module mentions {needle}");
    }

    // versions.tf: the provider is pinned exactly, and the backend is an
    // empty S3 block the app fills in.
    let terraform: Vec<_> = top_blocks(&body, "terraform").collect();
    assert_eq!(terraform.len(), 1);
    let tf = &terraform[0].body;
    assert_eq!(
        attr(tf, "required_version"),
        Some(&Expression::String(">= 1.10".into()))
    );
    let providers = top_blocks(tf, "required_providers").next().unwrap();
    let aws = attr(&providers.body, "aws").unwrap();
    assert_eq!(
        object_get(aws, "source"),
        Some(&Expression::String("hashicorp/aws".into()))
    );
    let Some(Expression::String(version)) = object_get(aws, "version") else {
        panic!("aws provider version is not a string")
    };
    let parts: Vec<&str> = version.split('.').collect();
    assert!(
        parts.len() == 3 && parts[0] == "6" && parts.iter().all(|p| p.parse::<u32>().is_ok()),
        "aws provider must be pinned to an exact 6.x version, got {version}"
    );
    let backend: Vec<_> = top_blocks(tf, "backend").collect();
    assert_eq!(backend.len(), 1);
    assert_eq!(label(backend[0], 0), Some("s3"));
    assert!(
        backend[0].body.iter().next().is_none(),
        "the backend block must be empty"
    );

    // The lock file ships with the module and pins the same version.
    let lock = module_files(kind)
        .iter()
        .find(|(name, _)| *name == ".terraform.lock.hcl")
        .unwrap_or_else(|| panic!("{kind:?}: lock file shipped"))
        .1;
    assert!(lock.contains("registry.opentofu.org/hashicorp/aws"));
    assert!(
        lock.contains(&format!("version     = \"{version}\"")),
        "lock pins {version}"
    );
}

#[test]
fn the_record_has_exactly_the_schema_fields() {
    let body = parsed(ModuleKind::Environment);
    let (param, rec) = record(&body);
    assert_eq!(
        object_keys(rec),
        RECORD_FIELDS.iter().map(|s| s.to_string()).collect()
    );
    assert_eq!(
        object_get(rec, "schema"),
        Some(&Expression::Number(1.into()))
    );
    assert_eq!(
        object_get(rec, "tenancy"),
        Some(&Expression::String("strict".into()))
    );
    assert_eq!(
        attr(&param.body, "type"),
        Some(&Expression::String("String".into()))
    );
    assert_eq!(
        render(attr(&param.body, "name").unwrap()),
        "\"/triple-c/environments/${var.environment_name}\""
    );
}

#[test]
fn every_variable_is_validated() {
    let body = parsed(ModuleKind::Environment);
    let vars: Vec<_> = top_blocks(&body, "variable").collect();
    let names: BTreeSet<_> = vars.iter().filter_map(|b| label(b, 0)).collect();
    assert_eq!(names, VARIABLES.iter().copied().collect());
    for v in vars {
        assert!(
            top_blocks(&v.body, "validation").next().is_some(),
            "variable {:?} has no validation",
            label(v, 0)
        );
    }
}

#[test]
fn module_files_ship_the_lock_file() {
    for (kind, extra) in [
        (ModuleKind::Environment, &["network.tf"][..]),
        (ModuleKind::UserOnboard, &["iam.tf", "efs.tf"][..]),
        (ModuleKind::PermissionsBoundary, &[][..]),
    ] {
        let names: Vec<_> = module_files(kind).iter().map(|(n, _)| *n).collect();
        for want in [
            "versions.tf",
            "variables.tf",
            "main.tf",
            "outputs.tf",
            ".terraform.lock.hcl",
        ]
        .iter()
        .chain(extra)
        {
            assert!(
                names.contains(want),
                "{kind:?}: {want} missing from {names:?}"
            );
        }
    }
}

#[test]
fn environment_vars_cover_every_network_mode() {
    let vars = environment_vars("dev", "us-west-2", &NetworkChoice::Public).unwrap();
    assert_eq!(
        vars,
        json!({
            "environment_name": "dev",
            "region": "us-west-2",
            "network_mode": "public",
            "vpc_id": "",
            "subnet_ids": [],
            "assign_public_ip": false,
        })
    );
    let vars = environment_vars("dev", "us-west-2", &NetworkChoice::PrivateNat).unwrap();
    assert_eq!(vars["network_mode"], "private-nat");
    assert_eq!(vars["subnet_ids"], json!([]));

    let existing = |vpc: &str, subnets: &[&str]| NetworkChoice::Existing {
        vpc_id: vpc.into(),
        subnet_ids: subnets.iter().map(|s| s.to_string()).collect(),
        assign_public_ip: true,
    };
    let two = ["subnet-0123456789abcdef0", "subnet-89abcdef"];
    let vars = environment_vars("dev", "eu-central-1", &existing("vpc-0a1b2c3d", &two)).unwrap();
    assert_eq!(
        vars,
        json!({
            "environment_name": "dev",
            "region": "eu-central-1",
            "network_mode": "existing",
            "vpc_id": "vpc-0a1b2c3d",
            "subnet_ids": two,
            "assign_public_ip": true,
        })
    );
    // Every variable the app sets is one the module declares.
    for key in vars.as_object().unwrap().keys() {
        assert!(VARIABLES.contains(&key.as_str()), "{key}");
    }

    for bad in [
        existing("vpc-0a1b2c3d", &["subnet-0123456789abcdef0"]),
        existing(
            "vpc-0a1b2c3d",
            &[
                "subnet-0123456789abcdef0",
                "subnet-89abcdef",
                "subnet-01234567",
            ],
        ),
        existing("vpc-0a1b2c3d", &["subnet-89abcdef", "subnet-89abcdef"]),
        existing("vpc-;", &two),
        existing("vpc-0A1B2C3D", &two),
        existing("", &two),
        existing("vpc-0a1b2c3d", &["subnet-0123456789abcdef0", "subnet-;"]),
        existing("vpc-0a1b2c3d", &["subnet-0123456789abcdef0", "sg-89abcdef"]),
    ] {
        assert!(
            environment_vars("dev", "us-west-2", &bad).is_err(),
            "{bad:?} accepted"
        );
    }
    assert!(environment_vars("Dev", "us-west-2", &NetworkChoice::Public).is_err());
    assert!(environment_vars("dev", "us-west-2;", &NetworkChoice::Public).is_err());
    assert!(environment_vars("dev", "cn-north-1", &NetworkChoice::Public).is_err());

    // The command layer receives the choice as tagged JSON.
    let parsed: NetworkChoice = serde_json::from_value(json!({
        "mode": "existing",
        "vpc_id": "vpc-0a1b2c3d",
        "subnet_ids": two,
        "assign_public_ip": false,
    }))
    .unwrap();
    assert!(matches!(parsed, NetworkChoice::Existing { .. }));
    let parsed: NetworkChoice = serde_json::from_value(json!({ "mode": "private-nat" })).unwrap();
    assert_eq!(parsed, NetworkChoice::PrivateNat);
}

// ---------------------------------------------------------------------------
// The onboarding and permissions-boundary modules.
// ---------------------------------------------------------------------------

const ONBOARD_VARIABLES: [&str; 5] = [
    "environment_name",
    "region",
    "owner_id",
    "owner_key",
    "bedrock_on_task_role",
];

/// The boundary ARN every module-created role carries, as the module writes it.
const BOUNDARY_ARN_TEMPLATE: &str = "arn:${data.aws_partition.current.partition}:iam::\
     ${data.aws_caller_identity.current.account_id}:policy/triple-c-permissions-boundary";

/// The template text of a string expression: a literal, or a quoted
/// template with its `${...}` interpolations kept verbatim.
fn template_text(expr: &Expression) -> String {
    match expr {
        Expression::String(s) => s.clone(),
        Expression::TemplateExpr(t) => t.to_string(),
        other => format!("${{{}}}", render(other)),
    }
}

/// `local.<name>`'s expression, from the module's (single) definition.
fn local_def<'a>(body: &'a Body, name: &str) -> &'a Expression {
    let defs: Vec<_> = top_blocks(body, "locals")
        .filter_map(|b| attr(&b.body, name))
        .collect();
    assert_eq!(defs.len(), 1, "local.{name} defined once");
    defs[0]
}

/// Template text with every whole `${local.x}` replaced by the local's own
/// text, repeatedly, so the result names only variables, data sources and
/// traversals into other locals (`${local.env.log_group}` stays as written).
fn expand_locals(body: &Body, text: &str) -> String {
    let mut out = text.to_string();
    let mut from = 0;
    for _ in 0..32 {
        let Some(found) = out[from..].find("${local.") else {
            return out;
        };
        let start = from + found;
        let name_start = start + "${local.".len();
        let name_len = out[name_start..]
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(out.len() - name_start);
        let name_end = name_start + name_len;
        if !out[name_end..].starts_with('}') {
            from = name_end;
            continue;
        }
        let replacement = match local_def(body, &out[name_start..name_end]) {
            e @ (Expression::String(_) | Expression::TemplateExpr(_)) => template_text(e),
            other => format!("${{{}}}", render(other)),
        };
        out = format!("{}{}{}", &out[..start], replacement, &out[name_end + 1..]);
        from = start;
    }
    panic!("locals nest too deeply in {text}")
}

/// An HCL value as JSON, keeping interpolations as template text
/// (`aws_iam_role.task.arn` becomes `"${aws_iam_role.task.arn}"`), so a
/// `jsonencode({...})` policy can be inspected like the JSON it produces.
fn to_json(expr: &Expression) -> serde_json::Value {
    use serde_json::Value;
    match expr {
        Expression::Null => Value::Null,
        Expression::Bool(b) => Value::Bool(*b),
        Expression::Number(n) => serde_json::to_value(n).unwrap(),
        Expression::Array(items) => Value::Array(items.iter().map(to_json).collect()),
        Expression::Object(obj) => {
            Value::Object(obj.iter().map(|(k, v)| (key_name(k), to_json(v))).collect())
        }
        other => Value::String(template_text(other)),
    }
}

/// The `jsonencode(...)` argument of `attr_name`, as JSON.
fn jsonencoded(block: &Block, attr_name: &str) -> serde_json::Value {
    let expr = attr(&block.body, attr_name)
        .unwrap_or_else(|| panic!("{:?} has no {attr_name}", block.labels));
    let Expression::FuncCall(call) = expr else {
        panic!("{:?}.{attr_name} is not jsonencode(...)", block.labels)
    };
    assert_eq!(call.name.name.as_str(), "jsonencode");
    to_json(&call.args[0])
}

fn statements(policy: &serde_json::Value) -> Vec<serde_json::Value> {
    assert_eq!(policy["Version"], "2012-10-17");
    policy["Statement"]
        .as_array()
        .expect("Statement list")
        .clone()
}

/// A statement's actions (a string or a list).
fn actions(stmt: &serde_json::Value) -> BTreeSet<String> {
    match &stmt["Action"] {
        serde_json::Value::String(a) => BTreeSet::from([a.clone()]),
        serde_json::Value::Array(a) => a.iter().map(|v| v.as_str().unwrap().to_string()).collect(),
        other => panic!("bad Action {other}"),
    }
}

fn resource(body: &Body, ty: &str, name: &str) -> Block {
    resources(body, ty)
        .into_iter()
        .find(|b| label(b, 1) == Some(name))
        .unwrap_or_else(|| panic!("{ty}.{name} missing"))
        .clone()
}

/// The inline policies attached to `aws_iam_role.<role>`, by resource name.
fn role_policies(body: &Body, role: &str) -> Vec<Block> {
    resources(body, "aws_iam_role_policy")
        .into_iter()
        .filter(|b| render(attr(&b.body, "role").unwrap()) == format!("aws_iam_role.{role}.id"))
        .cloned()
        .collect()
}

#[test]
fn every_onboarding_role_carries_the_boundary_and_owner_tag() {
    let body = parsed(ModuleKind::UserOnboard);

    // Owner, environment, component and version come from the provider's
    // default_tags, so every role (and everything else) carries them.
    let tags = default_tags(&body);
    assert_eq!(
        object_keys(tags),
        BTreeSet::from([
            "triple-c:component".to_string(),
            "triple-c:environment".to_string(),
            "triple-c:owner".to_string(),
            "triple-c:template-version".to_string(),
        ])
    );
    assert_eq!(
        object_get(tags, "triple-c:component"),
        Some(&Expression::String("user-onboarding".into()))
    );
    assert_eq!(
        render(object_get(tags, "triple-c:owner").unwrap()),
        "var.owner_id"
    );
    assert_eq!(
        render(object_get(tags, "triple-c:environment").unwrap()),
        "var.environment_name"
    );
    assert_eq!(
        tf_text(ModuleKind::UserOnboard)
            .matches("triple-c:component")
            .count(),
        1
    );
    for block in all_blocks(&body) {
        if block.identifier.as_str() == "default_tags" {
            continue;
        }
        for key in ["tags", "tags_all"] {
            if let Some(t) = attr(&block.body, key) {
                let keys = object_keys(t);
                assert!(
                    keys.iter().all(|k| !k.starts_with("triple-c:")),
                    "{:?} sets triple-c tags {keys:?}",
                    block.labels
                );
            }
        }
    }

    // Exactly the two per-user roles, named as the app names them, each
    // bounded; their permissions are inline only (nothing attached that the
    // module doesn't spell out).
    let roles = resources(&body, "aws_iam_role");
    assert_eq!(roles.len(), 2, "task and exec roles");
    let key = owner_key("AROAEXAMPLE:alice");
    let (task, exec) = user_role_names("dev", &key);
    for (name, want) in [("task", task), ("exec", exec)] {
        let role = resource(&body, "aws_iam_role", name);
        let named = expand_locals(&body, &template_text(attr(&role.body, "name").unwrap()))
            .replace("${var.environment_name}", "dev")
            .replace("${var.owner_key}", &key);
        assert_eq!(named, want);
        let boundary = attr(&role.body, "permissions_boundary")
            .unwrap_or_else(|| panic!("{name} role has no permissions_boundary"));
        assert_eq!(
            expand_locals(&body, &template_text(boundary)),
            BOUNDARY_ARN_TEMPLATE
        );
        assert!(attr(&role.body, "managed_policy_arns").is_none());
        assert!(top_blocks(&role.body, "inline_policy").next().is_none());
    }
    for ty in [
        "aws_iam_role_policy_attachment",
        "aws_iam_role_policy_attachments_exclusive",
        "aws_iam_policy_attachment",
        "aws_iam_policy",
        "aws_iam_user",
        "aws_iam_instance_profile",
    ] {
        assert!(resources(&body, ty).is_empty(), "{ty} present");
    }

    // The onboarding module's version agrees with the app's.
    let version = local_def(&body, "template_version");
    assert_eq!(version, &Expression::Number(ONBOARD_MODULE_VERSION.into()));
    assert!(
        render(object_get(tags, "triple-c:template-version").unwrap())
            .contains("local.template_version")
    );
}

#[test]
fn role_trust_is_ecs_tasks_in_this_account_only() {
    let body = parsed(ModuleKind::UserOnboard);
    for role in resources(&body, "aws_iam_role") {
        let trust = jsonencoded(role, "assume_role_policy");
        assert_eq!(
            statements(&trust),
            vec![json!({
                "Effect": "Allow",
                "Principal": { "Service": "ecs-tasks.amazonaws.com" },
                "Action": "sts:AssumeRole",
                "Condition": {
                    "StringEquals": {
                        "aws:SourceAccount": "${data.aws_caller_identity.current.account_id}"
                    }
                }
            })],
            "{:?}",
            role.labels
        );
    }
}

#[test]
fn the_users_file_system_admits_only_their_task_role_through_an_access_point() {
    let body = parsed(ModuleKind::UserOnboard);

    let systems = resources(&body, "aws_efs_file_system");
    assert_eq!(systems.len(), 1);
    let fs = systems[0];
    assert_eq!(label(fs, 1), Some("this"));
    assert_eq!(attr(&fs.body, "encrypted"), Some(&Expression::Bool(true)));
    assert_eq!(
        attr(&fs.body, "performance_mode"),
        Some(&Expression::String("generalPurpose".into()))
    );
    assert_eq!(
        attr(&fs.body, "throughput_mode"),
        Some(&Expression::String("elastic".into()))
    );

    // One mount target in each of the environment's two subnets.
    let targets = resources(&body, "aws_efs_mount_target");
    assert_eq!(targets.len(), 1);
    let mt = targets[0];
    assert_eq!(attr(&mt.body, "count"), Some(&Expression::Number(2.into())));
    assert_eq!(
        render(attr(&mt.body, "file_system_id").unwrap()),
        "aws_efs_file_system.this.id"
    );
    assert_eq!(
        render(attr(&mt.body, "subnet_id").unwrap()),
        "local.env.subnet_ids[count.index]"
    );
    assert_eq!(
        to_json(attr(&mt.body, "security_groups").unwrap()),
        json!(["${aws_security_group.efs.id}"])
    );

    // The file system policy.
    let policies = resources(&body, "aws_efs_file_system_policy");
    assert_eq!(policies.len(), 1);
    assert_eq!(
        render(attr(&policies[0].body, "file_system_id").unwrap()),
        "aws_efs_file_system.this.id"
    );
    let stmts = statements(&jsonencoded(policies[0], "policy"));
    let fs_arn = "${aws_efs_file_system.this.arn}";
    let deny_insecure = stmts
        .iter()
        .find(|s| s["Condition"]["Bool"]["aws:SecureTransport"] == "false")
        .expect("a SecureTransport deny");
    assert_eq!(deny_insecure["Effect"], "Deny");
    assert_eq!(deny_insecure["Principal"], json!({ "AWS": "*" }));
    assert_eq!(deny_insecure["Action"], "*");
    assert_eq!(deny_insecure["Resource"], fs_arn);

    let deny_direct = stmts
        .iter()
        .find(|s| s["Condition"]["Null"]["elasticfilesystem:AccessPointArn"] == "true")
        .expect("a deny without an access point");
    assert_eq!(deny_direct["Effect"], "Deny");
    assert_eq!(deny_direct["Principal"], json!({ "AWS": "*" }));
    assert!(
        actions(deny_direct).is_superset(&BTreeSet::from([
            "elasticfilesystem:ClientMount".to_string(),
            "elasticfilesystem:ClientWrite".to_string(),
            "elasticfilesystem:ClientRootAccess".to_string(),
        ])),
        "{deny_direct}"
    );
    assert_eq!(deny_direct["Resource"], fs_arn);

    let allows: Vec<_> = stmts.iter().filter(|s| s["Effect"] == "Allow").collect();
    assert_eq!(allows.len(), 1, "one Allow: {stmts:?}");
    assert_eq!(
        allows[0]["Principal"],
        json!({ "AWS": "${aws_iam_role.task.arn}" })
    );
    assert_eq!(
        actions(allows[0]),
        BTreeSet::from([
            "elasticfilesystem:ClientMount".to_string(),
            "elasticfilesystem:ClientWrite".to_string(),
        ])
    );
    assert_eq!(allows[0]["Resource"], fs_arn);
    assert!(allows[0].get("Condition").is_none() || allows[0]["Condition"].is_null());
    let deny_others = stmts
        .iter()
        .find(|s| s["Sid"] == "DenyAllButOwnersTaskRole")
        .expect("a deny for every principal but the task role");
    assert_eq!(
        deny_others,
        &json!({
            "Sid": "DenyAllButOwnersTaskRole",
            "Effect": "Deny",
            "Principal": { "AWS": "*" },
            "Action": [
                "elasticfilesystem:ClientMount",
                "elasticfilesystem:ClientWrite",
                "elasticfilesystem:ClientRootAccess",
            ],
            "Resource": fs_arn,
            "Condition": {
                "ArnNotEquals": { "aws:PrincipalArn": "${aws_iam_role.task.arn}" }
            },
        })
    );
    assert_eq!(stmts.len(), 4, "{stmts:?}");

    // The task role may mount this file system and no other.
    let efs: Vec<_> = role_policies(&body, "task")
        .iter()
        .flat_map(|p| statements(&jsonencoded(p, "policy")))
        .filter(|s| {
            actions(s)
                .iter()
                .any(|a| a.starts_with("elasticfilesystem:"))
        })
        .collect();
    assert_eq!(efs.len(), 1);
    assert_eq!(efs[0]["Resource"], fs_arn);
}

#[test]
fn the_environment_security_group_gains_no_ingress() {
    let body = parsed(ModuleKind::UserOnboard);
    for ty in [
        "aws_vpc_security_group_ingress_rule",
        "aws_vpc_security_group_egress_rule",
        "aws_security_group_rule",
        "aws_network_interface_sg_attachment",
    ] {
        assert!(resources(&body, ty).is_empty(), "{ty} present");
    }

    // The module's only security group is the user's mount-target group.
    let groups = resources(&body, "aws_security_group");
    assert_eq!(groups.len(), 1);
    let sg = groups[0];
    assert_eq!(label(sg, 1), Some("efs"));
    let key = owner_key("AROAEXAMPLE:alice");
    let name = expand_locals(&body, &template_text(attr(&sg.body, "name").unwrap()))
        .replace("${var.environment_name}", "dev")
        .replace("${var.owner_key}", &key);
    assert_eq!(name, format!("triple-c-dev-{key}-efs"));

    // Its single ingress rule: NFS from the environment's task group, taken
    // from the record. There is no other ingress anywhere in the module.
    let ingress: Vec<_> = all_blocks(&body)
        .into_iter()
        .filter(|b| b.identifier.as_str() == "ingress")
        .collect();
    assert_eq!(ingress.len(), 1, "exactly one ingress rule");
    assert_eq!(top_blocks(&sg.body, "ingress").count(), 1);
    let rule = &ingress[0].body;
    assert_eq!(
        attr(rule, "from_port"),
        Some(&Expression::Number(2049.into()))
    );
    assert_eq!(
        attr(rule, "to_port"),
        Some(&Expression::Number(2049.into()))
    );
    assert_eq!(
        attr(rule, "protocol"),
        Some(&Expression::String("tcp".into()))
    );
    assert_eq!(
        to_json(attr(rule, "security_groups").unwrap()),
        json!(["${local.env.security_group_id}"])
    );
    for open in ["cidr_blocks", "ipv6_cidr_blocks", "prefix_list_ids", "self"] {
        assert!(attr(rule, open).is_none(), "the NFS rule sets {open}");
    }

    // Mount targets never initiate connections (and groups are stateful), so
    // the group has no egress; `egress = []` removes any added out of band.
    assert_eq!(attr(&sg.body, "egress"), Some(&Expression::Array(vec![])));
    assert!(top_blocks(&sg.body, "egress").next().is_none());

    // The environment's group is referenced only as that rule's source.
    let text = tf_text(ModuleKind::UserOnboard);
    assert_eq!(text.matches("local.env.security_group_id").count(), 1);
    assert_eq!(
        text.matches("security_group_id").count(),
        4,
        "the rule, the record check and its message, and the efs group output"
    );
}

#[test]
fn exec_role_reads_only_its_owners_secrets() {
    let body = parsed(ModuleKind::UserOnboard);
    let account = "${data.aws_caller_identity.current.account_id}";
    let partition = "${data.aws_partition.current.partition}";
    let expand = |v: &serde_json::Value| expand_locals(&body, v.as_str().unwrap());

    let policies = role_policies(&body, "exec");
    assert_eq!(policies.len(), 1, "one exec policy");
    let stmts = statements(&jsonencoded(&policies[0], "policy"));
    let all: BTreeSet<String> = stmts.iter().flat_map(actions).collect();
    assert_eq!(
        all,
        BTreeSet::from([
            "secretsmanager:GetSecretValue".to_string(),
            "kms:Decrypt".to_string(),
            "logs:CreateLogStream".to_string(),
            "logs:PutLogEvents".to_string(),
        ])
    );
    assert!(stmts.iter().all(|s| s["Effect"] == "Allow"));

    let secrets = stmts
        .iter()
        .find(|s| actions(s).contains("secretsmanager:GetSecretValue"))
        .unwrap();
    assert_eq!(actions(secrets).len(), 1);
    assert_eq!(
        expand(&secrets["Resource"]),
        format!("arn:{partition}:secretsmanager:${{var.region}}:{account}:secret:triple-c/*")
    );
    assert_eq!(
        secrets["Condition"],
        json!({ "StringEquals": { "aws:ResourceTag/triple-c:owner": "${var.owner_id}" } })
    );

    let decrypt = stmts
        .iter()
        .find(|s| actions(s).contains("kms:Decrypt"))
        .unwrap();
    assert_eq!(actions(decrypt).len(), 1);
    assert_eq!(decrypt["Resource"], "${local.env.kms_key_arn}");
    assert_eq!(
        decrypt["Condition"],
        json!({ "StringEquals": { "kms:ViaService": "secretsmanager.${var.region}.amazonaws.com" } })
    );

    // Logs: the environment's log group only, for both roles.
    let log_group = format!(
        "arn:{partition}:logs:${{var.region}}:{account}:log-group:${{local.env.log_group}}:*"
    );
    for role in ["exec", "task"] {
        let logs: Vec<_> = role_policies(&body, role)
            .iter()
            .flat_map(|p| statements(&jsonencoded(p, "policy")))
            .filter(|s| actions(s).iter().any(|a| a.starts_with("logs:")))
            .collect();
        assert_eq!(logs.len(), 1, "{role}");
        assert_eq!(
            actions(&logs[0]),
            BTreeSet::from([
                "logs:CreateLogStream".to_string(),
                "logs:PutLogEvents".to_string()
            ])
        );
        assert_eq!(expand(&logs[0]["Resource"]), log_group, "{role}");
    }

    // The task role: exec channels, logs and its file system; Bedrock only
    // through a separate policy that exists when bedrock_on_task_role is set.
    let task = role_policies(&body, "task");
    assert_eq!(task.len(), 2, "task policy + optional bedrock policy");
    let mut always = BTreeSet::new();
    for p in &task {
        let acts: BTreeSet<String> = statements(&jsonencoded(p, "policy"))
            .iter()
            .flat_map(actions)
            .collect();
        if acts.iter().any(|a| a.starts_with("bedrock:")) {
            assert_eq!(
                render(attr(&p.body, "count").expect("bedrock policy is counted")),
                "var.bedrock_on_task_role ? 1 : 0"
            );
            assert!(
                acts.iter().all(|a| a.starts_with("bedrock:Invoke")),
                "{acts:?}"
            );
        } else {
            assert!(attr(&p.body, "count").is_none());
            always.extend(acts);
        }
    }
    assert_eq!(
        always,
        BTreeSet::from([
            "ssmmessages:CreateControlChannel".to_string(),
            "ssmmessages:CreateDataChannel".to_string(),
            "ssmmessages:OpenControlChannel".to_string(),
            "ssmmessages:OpenDataChannel".to_string(),
            "logs:CreateLogStream".to_string(),
            "logs:PutLogEvents".to_string(),
            "elasticfilesystem:ClientMount".to_string(),
            "elasticfilesystem:ClientWrite".to_string(),
        ])
    );
}

#[test]
fn onboarding_reads_the_environment_only_from_its_record() {
    let body = parsed(ModuleKind::UserOnboard);
    let params: Vec<_> = top_blocks(&body, "data")
        .filter(|b| label(b, 0) == Some("aws_ssm_parameter"))
        .collect();
    assert_eq!(params.len(), 1);
    assert_eq!(
        render(attr(&params[0].body, "name").unwrap()),
        "\"/triple-c/environments/${var.environment_name}\""
    );
    assert!(render(local_def(&body, "env")).starts_with("jsondecode(data.aws_ssm_parameter."));
    // No remote state, no other reads of the environment's deployment.
    let text = tf_text(ModuleKind::UserOnboard);
    assert!(!text.contains("terraform_remote_state"));
    assert!(resources(&body, "aws_ssm_parameter").is_empty());
}

#[test]
fn onboarding_vars_name_the_owner() {
    let owner = "AROAEXAMPLEID:alice@example.com";
    let vars = onboarding_vars("dev", "us-west-2", owner);
    assert_eq!(
        vars,
        json!({
            "environment_name": "dev",
            "region": "us-west-2",
            "owner_id": owner,
            "owner_key": owner_key(owner),
            "bedrock_on_task_role": false,
        })
    );
    let body = parsed(ModuleKind::UserOnboard);
    let declared: BTreeSet<_> = top_blocks(&body, "variable")
        .filter_map(|b| label(b, 0))
        .collect();
    assert_eq!(declared, ONBOARD_VARIABLES.iter().copied().collect());
    for v in top_blocks(&body, "variable") {
        if label(v, 0) != Some("bedrock_on_task_role") {
            assert!(
                top_blocks(&v.body, "validation").next().is_some(),
                "variable {:?} has no validation",
                label(v, 0)
            );
        }
    }
}

#[test]
fn boundary_vars_carry_the_shipped_boundary() {
    let vars = boundary_vars("us-west-2");
    assert_eq!(
        vars,
        json!({
            "region": "us-west-2",
            "policy_json": include_str!("../../../../../aws/policies/permissions-boundary.json"),
        })
    );
    assert_eq!(
        vars["policy_json"],
        crate::aws::policies::permissions_boundary_json()
    );

    let body = parsed(ModuleKind::PermissionsBoundary);
    let declared: BTreeSet<_> = top_blocks(&body, "variable")
        .filter_map(|b| label(b, 0))
        .collect();
    assert_eq!(declared, BTreeSet::from(["region", "policy_json"]));

    // One policy, no path, whose document is exactly what the app passes.
    let policies = resources(&body, "aws_iam_policy");
    assert_eq!(policies.len(), 1);
    let policy = policies[0];
    assert_eq!(
        attr(&policy.body, "name"),
        Some(&Expression::String(BOUNDARY_POLICY_NAME.into()))
    );
    assert!(
        attr(&policy.body, "path").is_none()
            || attr(&policy.body, "path") == Some(&Expression::String("/".into()))
    );
    assert_eq!(
        render(attr(&policy.body, "policy").unwrap()),
        "var.policy_json"
    );
    let resource_count = top_blocks(&body, "resource").count();
    assert_eq!(
        resource_count, 1,
        "the boundary module creates only the policy"
    );

    let tags = default_tags(&body);
    assert_eq!(
        object_keys(tags),
        BTreeSet::from(["triple-c:component".to_string()])
    );
    assert_eq!(
        object_get(tags, "triple-c:component"),
        Some(&Expression::String("boundary".into()))
    );
}

/// EC2 accepts only these characters in a security group's or rule's
/// description; the provider checks only the length, so a stray apostrophe
/// passes `tofu validate` and fails `CreateSecurityGroup`.
fn is_ec2_description(text: &str) -> bool {
    text.chars()
        .all(|c| c.is_ascii_alphanumeric() || " ._-:/()#,@[]+=&;{}!$*".contains(c))
}

#[test]
fn security_group_descriptions_use_only_ec2_characters() {
    assert!(is_ec2_description(
        "Triple-C ${var.environment_name}: NFS (tcp)"
    ));
    assert!(!is_ec2_description("the environment's tasks"));

    for kind in [ModuleKind::Environment, ModuleKind::UserOnboard] {
        let body = parsed(kind);
        let mut seen = 0;
        for ty in [
            "aws_security_group",
            "aws_security_group_rule",
            "aws_vpc_security_group_ingress_rule",
            "aws_vpc_security_group_egress_rule",
        ] {
            for res in resources(&body, ty) {
                let mut bodies = vec![&res.body];
                bodies.extend(
                    res.body
                        .blocks()
                        .filter(|b| matches!(b.identifier.as_str(), "ingress" | "egress"))
                        .map(|b| &b.body),
                );
                for b in bodies {
                    if let Some(d) = attr(b, "description") {
                        let text = template_text(d);
                        assert!(
                            is_ec2_description(&text),
                            "{kind:?} {:?}: description {text:?} has characters EC2 rejects",
                            res.labels
                        );
                        seen += 1;
                    }
                }
            }
        }
        assert!(seen >= 2, "{kind:?}: only {seen} descriptions checked");
    }
}

#[test]
fn the_record_is_format_checked_before_use() {
    let body = parsed(ModuleKind::UserOnboard);
    let param = top_blocks(&body, "data")
        .find(|b| label(b, 0) == Some("aws_ssm_parameter"))
        .unwrap();
    let lifecycle = top_blocks(&param.body, "lifecycle")
        .next()
        .expect("lifecycle");
    let conditions: String = top_blocks(&lifecycle.body, "postcondition")
        .map(|p| render(attr(&p.body, "condition").expect("condition")))
        .collect::<Vec<_>>()
        .join("\n");
    for needle in [
        "jsondecode(self.insecure_value).schema == 1",
        "jsondecode(self.insecure_value).name == var.environment_name",
        "jsondecode(self.insecure_value).tenancy == \"strict\"",
        "length(jsondecode(self.insecure_value).subnet_ids) == 2",
        "\"^arn:aws[a-z-]*:kms:${var.region}:${data.aws_caller_identity.current.account_id}:key/[0-9a-f-]+$\"",
        "jsondecode(self.insecure_value).kms_key_arn",
        "jsondecode(self.insecure_value).log_group == \"/triple-c/${var.environment_name}\"",
        "\"^sg-[0-9a-f]{8,17}$\"",
        "jsondecode(self.insecure_value).security_group_id",
        "\"^subnet-[0-9a-f]{8,17}$\"",
        // Every subnet, not just the first.
        "alltrue([for s in jsondecode(self.insecure_value).subnet_ids",
    ] {
        assert!(
            conditions.contains(needle),
            "the record postconditions do not check {needle}:\n{conditions}"
        );
    }

    // Every record field the module reads is one the postconditions check.
    let text = tf_text(ModuleKind::UserOnboard);
    let mut used = BTreeSet::new();
    for (i, _) in text.match_indices("local.env.") {
        let rest = &text[i + "local.env.".len()..];
        let end = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(rest.len());
        used.insert(rest[..end].to_string());
    }
    assert!(
        used.contains("kms_key_arn") && used.contains("subnet_ids"),
        "{used:?}"
    );
    for field in &used {
        assert!(
            conditions.contains(&format!("jsondecode(self.insecure_value).{field}")),
            "local.env.{field} is used but never checked"
        );
    }
}
