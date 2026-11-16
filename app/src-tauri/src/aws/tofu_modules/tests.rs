use std::collections::BTreeSet;

use hcl::expr::{Expression, ObjectKey};
use hcl::{Block, Body};
use serde_json::json;

use super::*;
use crate::aws::policies::PERMISSIONS_BOUNDARY_NAME;

const RECORD_FIELDS: [&str; 12] = [
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
    "max_workspace_hours",
];

const VARIABLES: [&str; 8] = [
    "environment_name",
    "region",
    "network_mode",
    "vpc_id",
    "subnet_ids",
    "assign_public_ip",
    "log_retention_days",
    "max_workspace_hours",
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
fn the_environment_cluster_and_log_group_names_match_the_module() {
    assert_eq!(environment_cluster_name("dev"), "triple-c-dev");
    assert_eq!(environment_log_group("dev"), "/triple-c/dev");
}

#[test]
fn same_user_and_environment_in_two_regions_do_not_collide() {
    // IAM role names are account-global but environments are regional: the
    // same user joining "dev" in two regions needs distinct roles (and
    // distinct onboarding state keys, though the buckets differ anyway).
    let user = "AROAEXAMPLEID:alice@example.com";
    let west = owner_key("us-west-2", user);
    let east = owner_key("us-east-1", user);
    assert_ne!(west, east);
    let (wt, we) = user_role_names("dev", &west);
    let (et, ee) = user_role_names("dev", &east);
    assert_ne!(wt, et);
    assert_ne!(we, ee);
    assert_ne!(
        state_key(Deployment::User { env: "dev", owner_key: &west }),
        state_key(Deployment::User { env: "dev", owner_key: &east })
    );
}

#[test]
fn owner_keys_are_stable_and_role_safe() {
    let a = owner_key("us-west-2", "AROAEXAMPLEID:alice@example.com");
    assert_eq!(a.len(), 12);
    assert!(
        a.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')),
        "{a}"
    );
    assert_eq!(a, owner_key("us-west-2", "AROAEXAMPLEID:alice@example.com"));
    assert_ne!(a, owner_key("us-west-2", "AROAEXAMPLEID:bob@example.com"));
    // The key is the first 12 hex characters of the SHA-256 of
    // "<region>:<user ID>".
    assert_eq!(
        owner_key("eu-west-1", "AIDAEXAMPLE"),
        sha256_prefix("eu-west-1:AIDAEXAMPLE")
    );

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
fn an_existing_network_is_checked_before_anything_is_built() {
    // `existing` takes the user's VPC and subnets on trust otherwise: the
    // module looks the subnets up and refuses ones outside var.vpc_id or in
    // one availability zone (the onboarding module's own check), before the
    // security group, the first thing built in that VPC.
    let body = parsed(ModuleKind::Environment);
    let lookups: Vec<_> = top_blocks(&body, "data")
        .filter(|b| label(b, 0) == Some("aws_subnet"))
        .collect();
    assert_eq!(lookups.len(), 1, "one aws_subnet lookup");
    let lookup = lookups[0];
    assert_eq!(label(lookup, 1), Some("existing"));
    let count = render(attr(&lookup.body, "count").expect("count"));
    assert!(count.contains("var.network_mode") && count.contains("\"existing\""), "{count}");
    assert!(count.contains('2'), "{count}");
    let id = render(attr(&lookup.body, "id").expect("id"));
    assert!(id.contains("var.subnet_ids[count.index]"), "{id}");

    let sg = resource(&body, "aws_security_group", "tasks");
    let lifecycle = top_blocks(&sg.body, "lifecycle").next().expect("lifecycle");
    let pre = top_blocks(&lifecycle.body, "precondition")
        .next()
        .expect("a precondition on the task security group");
    let cond = render(attr(&pre.body, "condition").expect("condition"));
    for needle in [
        "data.aws_subnet.existing",
        ".vpc_id == var.vpc_id",
        "availability_zone",
        "distinct",
    ] {
        assert!(cond.contains(needle), "{needle} missing from {cond}");
    }
    assert!(attr(&pre.body, "error_message").is_some());
}

#[test]
fn the_onboarding_completion_output_waits_for_every_resource() {
    // Detection reads Joined only from a state holding this output, so it
    // must depend on every resource the module makes.
    let body = parsed(ModuleKind::UserOnboard);
    let out = top_blocks(&body, "output")
        .find(|b| label(b, 0) == Some("onboarding_complete"))
        .expect("an onboarding_complete output");
    assert_eq!(attr(&out.body, "value"), Some(&Expression::Bool(true)));
    let deps = render(attr(&out.body, "depends_on").expect("depends_on"));
    let mut n = 0;
    for r in top_blocks(&body, "resource") {
        let addr = format!("{}.{}", label(r, 0).unwrap(), label(r, 1).unwrap());
        assert!(deps.contains(&addr), "onboarding_complete doesn't wait for {addr}: {deps}");
        n += 1;
    }
    assert!(n >= 9, "only {n} resources");
}

#[test]
fn the_module_versions_agree() {
    assert_eq!(ENVIRONMENT_MODULE_VERSION, 2);
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
    let vars = environment_vars("dev", "us-west-2", &NetworkChoice::Public, 0).unwrap();
    assert_eq!(
        vars,
        json!({
            "environment_name": "dev",
            "region": "us-west-2",
            "network_mode": "public",
            "vpc_id": "",
            "subnet_ids": [],
            "assign_public_ip": false,
            "max_workspace_hours": 0,
        })
    );
    let vars = environment_vars("dev", "us-west-2", &NetworkChoice::PrivateNat, 8).unwrap();
    assert_eq!(vars["max_workspace_hours"], 8);
    assert_eq!(vars["network_mode"], "private-nat");
    assert_eq!(vars["subnet_ids"], json!([]));

    let existing = |vpc: &str, subnets: &[&str]| NetworkChoice::Existing {
        vpc_id: vpc.into(),
        subnet_ids: subnets.iter().map(|s| s.to_string()).collect(),
        assign_public_ip: true,
    };
    let two = ["subnet-0123456789abcdef0", "subnet-89abcdef"];
    let vars =
        environment_vars("dev", "eu-central-1", &existing("vpc-0a1b2c3d", &two), 168).unwrap();
    assert_eq!(
        vars,
        json!({
            "environment_name": "dev",
            "region": "eu-central-1",
            "network_mode": "existing",
            "vpc_id": "vpc-0a1b2c3d",
            "subnet_ids": two,
            "assign_public_ip": true,
            "max_workspace_hours": 168,
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
            environment_vars("dev", "us-west-2", &bad, 0).is_err(),
            "{bad:?} accepted"
        );
    }
    assert!(environment_vars("Dev", "us-west-2", &NetworkChoice::Public, 0).is_err());
    assert!(environment_vars("dev", "us-west-2;", &NetworkChoice::Public, 0).is_err());
    assert!(environment_vars("dev", "cn-north-1", &NetworkChoice::Public, 0).is_err());
    assert!(environment_vars("dev", "us-west-2", &NetworkChoice::Public, 169).is_err());

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
    let key = owner_key("us-west-2", "AROAEXAMPLE:alice");
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
    let key = owner_key("us-west-2", "AROAEXAMPLE:alice");
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
            "owner_key": owner_key("us-west-2", owner),
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
        BTreeSet::from([
            "triple-c:component".to_string(),
            "triple-c:template-version".to_string()
        ])
    );
    assert_eq!(
        object_get(tags, "triple-c:component"),
        Some(&Expression::String("boundary".into()))
    );
}

#[test]
fn the_boundary_module_version_is_pinned() {
    assert_eq!(BOUNDARY_MODULE_VERSION, 2);
    let body = parsed(ModuleKind::PermissionsBoundary);
    let version = local_def(&body, "template_version");
    assert_eq!(version, &Expression::Number(BOUNDARY_MODULE_VERSION.into()));
    let tag = object_get(default_tags(&body), "triple-c:template-version")
        .expect("template-version default tag");
    assert!(
        render(tag).contains("local.template_version"),
        "template-version tag is {}",
        render(tag)
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

// ---------------------------------------------------------------------------
// What the admin tier must allow for the modules to apply.
//
// MODULE_CALLS maps every Terraform resource type (and data source, and the
// S3 backend) the environment and onboarding modules use to the IAM actions
// the AWS provider calls to create, read (refresh), tag, update and delete
// it, each with the resource it is authorized against and the context IAM
// sees. It is the hypothesis: Task 8's live run is the proof, and an action
// it shows missing is added here, to the policy, to the check table
// (`policies.rs`) and to `aws/README.md` together.

use crate::aws::policies::{
    checks, permissions_boundary_json, policy_files, Check, ContextValue, Tier,
};
use crate::aws::policy_eval as eval;

type Context = &'static [(&'static str, ContextValue)];

const fn call(action: &'static str, resource: &'static str, context: Context) -> Check {
    Check {
        action,
        resource,
        context,
        write: true,
    }
}

// The resources the modules create for environment `probe` and the user
// whose owner key is `0123456789ab`, named as Task 1 names them (pinned to
// the naming functions by `the_module_call_table_uses_the_shared_names`).
const ENV: &str = "probe";
const OWNER_KEY: &str = "0123456789ab";
const CLUSTER: &str = "arn:aws:ecs:{region}:{account}:cluster/triple-c-probe";
const KEY: &str = "arn:aws:kms:{region}:{account}:key/00000000-0000-0000-0000-000000000000";
const ALIAS: &str = "arn:aws:kms:{region}:{account}:alias/triple-c-probe";
const LOG_GROUP: &str = "arn:aws:logs:{region}:{account}:log-group:/triple-c/probe";
const PARAMETER: &str = "arn:aws:ssm:{region}:{account}:parameter/triple-c/environments/probe";
const VPC: &str = "arn:aws:ec2:{region}:{account}:vpc/vpc-00000000000000000";
const SUBNET: &str = "arn:aws:ec2:{region}:{account}:subnet/subnet-00000000000000000";
const IGW: &str = "arn:aws:ec2:{region}:{account}:internet-gateway/igw-00000000000000000";
const RTB: &str = "arn:aws:ec2:{region}:{account}:route-table/rtb-00000000000000000";
const EIP: &str = "arn:aws:ec2:{region}:{account}:elastic-ip/eipalloc-00000000000000000";
const NAT: &str = "arn:aws:ec2:{region}:{account}:natgateway/nat-00000000000000000";
const SG: &str = "arn:aws:ec2:{region}:{account}:security-group/sg-00000000000000000";
const SG_RULE: &str = "arn:aws:ec2:{region}:{account}:security-group-rule/sgr-00000000000000000";
const GONE_ASSOCIATION: &str = "arn:aws:ec2:{region}:{account}:*/*";
const ENI: &str = "arn:aws:ec2:{region}:{account}:network-interface/eni-00000000000000000";
const FS: &str = "arn:aws:elasticfilesystem:{region}:{account}:file-system/fs-00000000";
const TASK_ROLE: &str = "arn:aws:iam::{account}:role/triple-c-probe-0123456789ab-task";
const EXEC_ROLE: &str = "arn:aws:iam::{account}:role/triple-c-probe-0123456789ab-exec";
const STATE_BUCKET: &str = "arn:aws:s3:::triple-c-tfstate-{account}-{region}";
const ENV_STATE: &str =
    "arn:aws:s3:::triple-c-tfstate-{account}-{region}/environments/probe/terraform.tfstate";
const ENV_LOCK: &str =
    "arn:aws:s3:::triple-c-tfstate-{account}-{region}/environments/probe/terraform.tfstate.tflock";
const USER_STATE: &str =
    "arn:aws:s3:::triple-c-tfstate-{account}-{region}/users/probe/0123456789ab/terraform.tfstate";
const USER_LOCK: &str = "arn:aws:s3:::triple-c-tfstate-{account}-{region}/users/probe/0123456789ab/terraform.tfstate.tflock";

use ContextValue::Literal;

const REQUEST_COMPONENT: &str = "aws:RequestTag/triple-c:component";
const RESOURCE_COMPONENT: &str = "aws:ResourceTag/triple-c:component";
/// Created by the environment module: default_tags put the component tag on
/// the create request.
const ENV_CREATE: Context = &[(REQUEST_COMPONENT, Literal("environment"))];
/// An environment-module resource after creation.
const ENV_TAGGED: Context = &[(RESOURCE_COMPONENT, Literal("environment"))];
const USER_CREATE: Context = &[(REQUEST_COMPONENT, Literal("user-onboarding"))];
const USER_TAGGED: Context = &[(RESOURCE_COMPONENT, Literal("user-onboarding"))];
/// Role writes name the boundary the module sets.
const BOUNDED: Context = &[
    (
        "iam:PermissionsBoundary",
        Literal("arn:aws:iam::{account}:policy/triple-c-permissions-boundary"),
    ),
    ("aws:PrincipalAccount", Literal("{account}")),
];
/// Created inside an environment-module VPC, subnet or address: the new
/// resource carries the request tag, its parent the component tag.
const ENV_CREATE_IN_ENV: Context = &[
    (REQUEST_COMPONENT, Literal("environment")),
    (RESOURCE_COMPONENT, Literal("environment")),
];
/// A call EFS makes on the caller's behalf (a mount target's interface).
const VIA_SERVICE: Context = &[(
    "aws:CalledViaLast",
    Literal("elasticfilesystem.amazonaws.com"),
)];
const NONE: Context = &[];

macro_rules! created_by {
    ($action:literal) => {
        &[("ec2:CreateAction", Literal($action))]
    };
}

/// The state bucket is in the caller's own account.
const IN_ACCOUNT: Context = &[
    ("s3:ResourceAccount", Literal("{account}")),
    ("aws:PrincipalAccount", Literal("{account}")),
];

/// The S3 backend's calls on one deployment's state and lock objects.
const BACKEND: &[Check] = &[
    call("s3:ListBucket", STATE_BUCKET, IN_ACCOUNT),
    call("s3:GetObject", ENV_STATE, IN_ACCOUNT),
    call("s3:PutObject", ENV_STATE, IN_ACCOUNT),
    call("s3:GetObject", ENV_LOCK, IN_ACCOUNT),
    call("s3:PutObject", ENV_LOCK, IN_ACCOUNT),
    call("s3:DeleteObject", ENV_LOCK, IN_ACCOUNT),
    call("s3:GetObject", USER_STATE, IN_ACCOUNT),
    call("s3:PutObject", USER_STATE, IN_ACCOUNT),
    call("s3:PutObject", USER_LOCK, IN_ACCOUNT),
    call("s3:DeleteObject", USER_LOCK, IN_ACCOUNT),
];

const MODULE_CALLS: &[(&str, &[Check])] = &[
    (
        "aws_ecs_cluster",
        &[
            call("ecs:CreateCluster", CLUSTER, ENV_CREATE),
            call(
                "ecs:TagResource",
                CLUSTER,
                &[("ecs:CreateAction", Literal("CreateCluster"))],
            ),
            call("ecs:DescribeClusters", CLUSTER, NONE),
            call("ecs:ListTagsForResource", CLUSTER, NONE),
            call("ecs:UpdateCluster", CLUSTER, ENV_TAGGED),
            call("ecs:TagResource", CLUSTER, ENV_TAGGED),
            call("ecs:UntagResource", CLUSTER, ENV_TAGGED),
            call("ecs:DeleteCluster", CLUSTER, ENV_TAGGED),
        ],
    ),
    (
        // Untaggable: authorized against the (named) cluster.
        "aws_ecs_cluster_capacity_providers",
        &[
            call("ecs:PutClusterCapacityProviders", CLUSTER, ENV_TAGGED),
            call("ecs:DescribeClusters", CLUSTER, NONE),
        ],
    ),
    (
        // Both modules' groups. The provider revokes AWS's default egress
        // rule after creating (both groups declare their own egress), and
        // authorizes rules one call per direction, against the group and the
        // new (untagged) rule.
        "aws_security_group",
        &[
            call("ec2:CreateSecurityGroup", SG, ENV_CREATE),
            // The one create that may name an untagged VPC: an
            // `existing`-network environment's is the user's own. EC2 shows
            // the VPC no request tag (verified live).
            call("ec2:CreateSecurityGroup", VPC, NONE),
            call("ec2:CreateTags", SG, created_by!("CreateSecurityGroup")),
            call("ec2:DescribeSecurityGroups", "*", NONE),
            call("ec2:DescribeSecurityGroupRules", "*", NONE),
            call("ec2:RevokeSecurityGroupEgress", SG, USER_TAGGED),
            call("ec2:RevokeSecurityGroupEgress", SG, ENV_TAGGED),
            call("ec2:AuthorizeSecurityGroupEgress", SG, ENV_TAGGED),
            call("ec2:AuthorizeSecurityGroupEgress", SG_RULE, NONE),
            call("ec2:AuthorizeSecurityGroupIngress", SG, USER_TAGGED),
            call("ec2:AuthorizeSecurityGroupIngress", SG_RULE, NONE),
            call("ec2:RevokeSecurityGroupIngress", SG, ENV_TAGGED),
            call(
                "ec2:UpdateSecurityGroupRuleDescriptionsIngress",
                SG,
                USER_TAGGED,
            ),
            call(
                "ec2:UpdateSecurityGroupRuleDescriptionsEgress",
                SG,
                ENV_TAGGED,
            ),
            call("ec2:CreateTags", SG, ENV_TAGGED),
            call("ec2:DeleteTags", SG, ENV_TAGGED),
            call("ec2:DescribeNetworkInterfaces", "*", NONE),
            call("ec2:DeleteSecurityGroup", SG, ENV_TAGGED),
            call("ec2:DeleteSecurityGroup", SG, USER_TAGGED),
        ],
    ),
    (
        // Tag-on-create authorizes kms:TagResource against the new key with
        // its request tags (verified live in phase 3).
        "aws_kms_key",
        &[
            call("kms:CreateKey", "*", ENV_CREATE),
            call("kms:TagResource", KEY, ENV_TAGGED),
            call("kms:DescribeKey", KEY, NONE),
            call("kms:GetKeyPolicy", KEY, NONE),
            call("kms:GetKeyRotationStatus", KEY, NONE),
            call("kms:ListResourceTags", KEY, NONE),
            call("kms:EnableKeyRotation", KEY, ENV_TAGGED),
            call("kms:PutKeyPolicy", KEY, ENV_TAGGED),
            call("kms:UpdateKeyDescription", KEY, ENV_TAGGED),
            call("kms:UntagResource", KEY, ENV_TAGGED),
            call("kms:ScheduleKeyDeletion", KEY, ENV_TAGGED),
        ],
    ),
    (
        // Untaggable: authorized against the alias (by name) and the key.
        "aws_kms_alias",
        &[
            call("kms:CreateAlias", ALIAS, NONE),
            call("kms:CreateAlias", KEY, ENV_TAGGED),
            call("kms:ListAliases", "*", NONE),
            call("kms:UpdateAlias", ALIAS, NONE),
            call("kms:UpdateAlias", KEY, ENV_TAGGED),
            call("kms:DeleteAlias", ALIAS, NONE),
            call("kms:DeleteAlias", KEY, ENV_TAGGED),
        ],
    ),
    (
        "aws_cloudwatch_log_group",
        &[
            call("logs:CreateLogGroup", LOG_GROUP, ENV_CREATE),
            call("logs:TagResource", LOG_GROUP, ENV_CREATE),
            call("logs:PutRetentionPolicy", LOG_GROUP, NONE),
            call("logs:DescribeLogGroups", "*", NONE),
            call("logs:ListTagsForResource", LOG_GROUP, NONE),
            call("logs:UntagResource", LOG_GROUP, NONE),
            call("logs:DeleteRetentionPolicy", LOG_GROUP, NONE),
            call("logs:DeleteLogGroup", LOG_GROUP, NONE),
        ],
    ),
    (
        "aws_ssm_parameter",
        &[
            call("ssm:PutParameter", PARAMETER, ENV_CREATE),
            call("ssm:AddTagsToResource", PARAMETER, ENV_CREATE),
            call("ssm:GetParameter", PARAMETER, NONE),
            call("ssm:DescribeParameters", "*", NONE),
            call("ssm:ListTagsForResource", PARAMETER, NONE),
            call("ssm:RemoveTagsFromResource", PARAMETER, NONE),
            call("ssm:DeleteParameter", PARAMETER, NONE),
        ],
    ),
    (
        // enable_dns_hostnames is set after CreateVpc; reads include the
        // default ACL, route table and security group.
        "aws_vpc",
        &[
            call("ec2:CreateVpc", VPC, ENV_CREATE),
            call("ec2:CreateTags", VPC, created_by!("CreateVpc")),
            call("ec2:DescribeVpcs", "*", NONE),
            call("ec2:DescribeVpcAttribute", "*", NONE),
            call("ec2:DescribeNetworkAcls", "*", NONE),
            call("ec2:DescribeRouteTables", "*", NONE),
            call("ec2:ModifyVpcAttribute", VPC, ENV_TAGGED),
            call("ec2:CreateTags", VPC, ENV_TAGGED),
            call("ec2:DeleteTags", VPC, ENV_TAGGED),
            call("ec2:DeleteVpc", VPC, ENV_TAGGED),
        ],
    ),
    (
        "aws_internet_gateway",
        &[
            call("ec2:CreateInternetGateway", IGW, ENV_CREATE),
            call("ec2:CreateTags", IGW, created_by!("CreateInternetGateway")),
            call("ec2:AttachInternetGateway", IGW, ENV_TAGGED),
            call("ec2:AttachInternetGateway", VPC, ENV_TAGGED),
            call("ec2:DescribeInternetGateways", "*", NONE),
            call("ec2:CreateTags", IGW, ENV_TAGGED),
            call("ec2:DetachInternetGateway", IGW, ENV_TAGGED),
            call("ec2:DetachInternetGateway", VPC, ENV_TAGGED),
            call("ec2:DeleteInternetGateway", IGW, ENV_TAGGED),
        ],
    ),
    (
        // Deleting waits for the subnet's interfaces to go.
        "aws_subnet",
        &[
            call("ec2:CreateSubnet", SUBNET, ENV_CREATE),
            call("ec2:CreateSubnet", VPC, ENV_CREATE_IN_ENV),
            call("ec2:CreateTags", SUBNET, created_by!("CreateSubnet")),
            call("ec2:DescribeSubnets", "*", NONE),
            call("ec2:ModifySubnetAttribute", SUBNET, ENV_TAGGED),
            call("ec2:CreateTags", SUBNET, ENV_TAGGED),
            call("ec2:DeleteTags", SUBNET, ENV_TAGGED),
            call("ec2:DescribeNetworkInterfaces", "*", NONE),
            call("ec2:DeleteSubnet", SUBNET, ENV_TAGGED),
        ],
    ),
    (
        "aws_route_table",
        &[
            call("ec2:CreateRouteTable", RTB, ENV_CREATE),
            call("ec2:CreateRouteTable", VPC, ENV_CREATE_IN_ENV),
            call("ec2:CreateTags", RTB, created_by!("CreateRouteTable")),
            call("ec2:DescribeRouteTables", "*", NONE),
            call("ec2:CreateTags", RTB, ENV_TAGGED),
            call("ec2:DeleteRouteTable", RTB, ENV_TAGGED),
        ],
    ),
    (
        // Untaggable: authorized against the route table.
        "aws_route",
        &[
            call("ec2:CreateRoute", RTB, ENV_TAGGED),
            call("ec2:DescribeRouteTables", "*", NONE),
            call("ec2:ReplaceRoute", RTB, ENV_TAGGED),
            call("ec2:DeleteRoute", RTB, ENV_TAGGED),
        ],
    ),
    (
        // Untaggable: authorized against the route table and the subnet.
        // Replacing an association is never allowed (it could swap a VPC's
        // main route table), so a changed route_table_id fails rather than
        // re-associating; the modules never change it.
        "aws_route_table_association",
        &[
            call("ec2:AssociateRouteTable", RTB, ENV_TAGGED),
            call("ec2:AssociateRouteTable", SUBNET, ENV_TAGGED),
            call("ec2:DescribeRouteTables", "*", NONE),
            call("ec2:DisassociateRouteTable", RTB, ENV_TAGGED),
            call("ec2:DisassociateRouteTable", SUBNET, ENV_TAGGED),
        ],
    ),
    (
        "aws_eip",
        &[
            call("ec2:AllocateAddress", EIP, ENV_CREATE),
            call("ec2:CreateTags", EIP, created_by!("AllocateAddress")),
            call("ec2:DescribeAddresses", "*", NONE),
            call("ec2:DescribeAddressesAttribute", "*", NONE),
            call("ec2:CreateTags", EIP, ENV_TAGGED),
            // Delete disassociates first. The NAT gateway that held the
            // association is gone by then, so EC2 authorizes it against a
            // literal `*/*` (verified live).
            call("ec2:DisassociateAddress", GONE_ASSOCIATION, NONE),
            call("ec2:ReleaseAddress", EIP, ENV_TAGGED),
        ],
    ),
    (
        // Authorized against the gateway, its subnet, its address and the VPC.
        "aws_nat_gateway",
        &[
            call("ec2:CreateNatGateway", NAT, ENV_CREATE),
            call("ec2:CreateNatGateway", SUBNET, ENV_CREATE_IN_ENV),
            call("ec2:CreateNatGateway", EIP, ENV_CREATE_IN_ENV),
            call("ec2:CreateNatGateway", VPC, ENV_CREATE_IN_ENV),
            call("ec2:CreateTags", NAT, created_by!("CreateNatGateway")),
            call("ec2:DescribeNatGateways", "*", NONE),
            call("ec2:CreateTags", NAT, ENV_TAGGED),
            call("ec2:DeleteNatGateway", NAT, ENV_TAGGED),
        ],
    ),
    (
        "aws_efs_file_system",
        &[
            call("elasticfilesystem:CreateFileSystem", "*", USER_CREATE),
            call(
                "elasticfilesystem:TagResource",
                FS,
                &[(
                    "elasticfilesystem:CreateAction",
                    Literal("CreateFileSystem"),
                )],
            ),
            call("elasticfilesystem:DescribeFileSystems", FS, NONE),
            call("elasticfilesystem:DescribeLifecycleConfiguration", FS, NONE),
            call("elasticfilesystem:ListTagsForResource", FS, NONE),
            call("elasticfilesystem:UpdateFileSystem", FS, USER_TAGGED),
            call(
                "elasticfilesystem:PutLifecycleConfiguration",
                FS,
                USER_TAGGED,
            ),
            call("elasticfilesystem:TagResource", FS, USER_TAGGED),
            call("elasticfilesystem:UntagResource", FS, USER_TAGGED),
            call("elasticfilesystem:DeleteFileSystem", FS, USER_TAGGED),
        ],
    ),
    (
        // Untaggable here (the module sets no tags on it). EFS creates and
        // deletes the mount target's interface with the caller's EC2
        // permissions (CreateMountTarget / DeleteMountTarget API reference).
        "aws_efs_mount_target",
        &[
            call("elasticfilesystem:CreateMountTarget", FS, USER_TAGGED),
            call("ec2:DescribeSubnets", "*", NONE),
            call("ec2:DescribeNetworkInterfaces", "*", NONE),
            call("ec2:CreateNetworkInterface", ENI, VIA_SERVICE),
            call("ec2:CreateNetworkInterface", SUBNET, VIA_SERVICE),
            call("ec2:CreateNetworkInterface", SG, VIA_SERVICE),
            call("elasticfilesystem:DescribeMountTargets", FS, NONE),
            call(
                "elasticfilesystem:DescribeMountTargetSecurityGroups",
                FS,
                NONE,
            ),
            call(
                "elasticfilesystem:ModifyMountTargetSecurityGroups",
                FS,
                USER_TAGGED,
            ),
            call("elasticfilesystem:DeleteMountTarget", FS, USER_TAGGED),
            call("ec2:DeleteNetworkInterface", ENI, VIA_SERVICE),
        ],
    ),
    (
        // Untaggable: authorized against the file system.
        "aws_efs_file_system_policy",
        &[
            call("elasticfilesystem:PutFileSystemPolicy", FS, USER_TAGGED),
            call("elasticfilesystem:DescribeFileSystemPolicy", FS, NONE),
            call("elasticfilesystem:DeleteFileSystemPolicy", FS, USER_TAGGED),
        ],
    ),
    (
        // Deleting a role first lists (and removes) its instance profiles
        // and inline policies.
        "aws_iam_role",
        &[
            call("iam:CreateRole", TASK_ROLE, BOUNDED),
            call("iam:CreateRole", EXEC_ROLE, BOUNDED),
            call("iam:TagRole", TASK_ROLE, NONE),
            call("iam:GetRole", TASK_ROLE, NONE),
            call("iam:ListRolePolicies", TASK_ROLE, NONE),
            call("iam:ListAttachedRolePolicies", TASK_ROLE, NONE),
            call("iam:ListInstanceProfilesForRole", TASK_ROLE, NONE),
            call("iam:UpdateRole", TASK_ROLE, NONE),
            call("iam:UpdateRoleDescription", TASK_ROLE, NONE),
            call("iam:UntagRole", TASK_ROLE, NONE),
            call("iam:DeleteRole", EXEC_ROLE, NONE),
        ],
    ),
    (
        // Untaggable: authorized against the role.
        "aws_iam_role_policy",
        &[
            call("iam:PutRolePolicy", TASK_ROLE, BOUNDED),
            call("iam:GetRolePolicy", TASK_ROLE, NONE),
            call("iam:DeleteRolePolicy", EXEC_ROLE, BOUNDED),
        ],
    ),
    (
        "data.aws_caller_identity",
        &[call("sts:GetCallerIdentity", "*", NONE)],
    ),
    ("data.aws_partition", &[]),
    (
        "data.aws_availability_zones",
        &[call("ec2:DescribeAvailabilityZones", "*", NONE)],
    ),
    (
        "data.aws_ssm_parameter",
        &[call("ssm:GetParameter", PARAMETER, NONE)],
    ),
    ("data.aws_subnet", &[call("ec2:DescribeSubnets", "*", NONE)]),
    ("backend.s3", BACKEND),
];

// Phase 7 (per-user EC2 capacity, E1–E5, E11; fix round 1): the calls the onboarding
// module's EC2 resources will make, written down with the policy (Task 2)
// before the resources exist. Task 3 moves each entry into MODULE_CALLS
// under its type (merging with the existing `aws_ecs_cluster`,
// `aws_ecs_cluster_capacity_providers`, `aws_iam_role`, `aws_ssm_parameter`
// and `data.aws_ssm_parameter` entries) and deletes this table. Names are
// the user's (`triple-c-<env>-<owner key>[-instance]`).
const USER_CLUSTER: &str = "arn:aws:ecs:{region}:{account}:cluster/triple-c-probe-0123456789ab";
const CAPACITY_PROVIDER: &str =
    "arn:aws:ecs:{region}:{account}:capacity-provider/triple-c-probe-0123456789ab";
const LAUNCH_TEMPLATE: &str = "arn:aws:ec2:{region}:{account}:launch-template/lt-00000000000000000";
const AUTO_SCALING_GROUP: &str = "arn:aws:autoscaling:{region}:{account}:autoScalingGroup:\
     00000000-0000-0000-0000-000000000000:autoScalingGroupName/triple-c-probe-0123456789ab";
const INSTANCE: &str = "arn:aws:ec2:{region}:{account}:instance/i-00000000000000000";
const VOLUME: &str = "arn:aws:ec2:{region}:{account}:volume/vol-00000000000000000";
const IMAGE: &str = "arn:aws:ec2:{region}::image/ami-00000000000000000";
const INSTANCE_ROLE: &str = "arn:aws:iam::{account}:role/triple-c-probe-0123456789ab-instance";
const INSTANCE_PROFILE: &str =
    "arn:aws:iam::{account}:instance-profile/triple-c-probe-0123456789ab-instance";
const AUTO_SCALING_SERVICE_ROLE: &str = "arn:aws:iam::{account}:role/aws-service-role/\
     autoscaling.amazonaws.com/AWSServiceRoleForAutoScaling";
const ECS_SERVICE_ROLE: &str =
    "arn:aws:iam::{account}:role/aws-service-role/ecs.amazonaws.com/AWSServiceRoleForECS";
const AMI_PARAMETER: &str = "arn:aws:ssm:{region}::parameter/aws/service/ecs/optimized-ami/\
     amazon-linux-2023/recommended/image_id";
const USER_RECORD: &str =
    "arn:aws:ssm:{region}:{account}:parameter/triple-c/users/probe/0123456789ab";
/// A launch from the user's launch template (what creating the group
/// checks the caller for).
const FROM_TEMPLATE: Context = &[(
    "ec2:LaunchTemplate",
    Literal("arn:aws:ec2:{region}:{account}:launch-template/lt-00000000000000000"),
)];
const LT_ARN: ContextValue =
    Literal("arn:aws:ec2:{region}:{account}:launch-template/lt-00000000000000000");
/// The template's tag specifications tag the instance and its volume.
const TAGGED_INSTANCE: Context = &[
    (REQUEST_COMPONENT, Literal("user-onboarding")),
    ("ec2:LaunchTemplate", LT_ARN),
    ("ec2:InstanceType", Literal("m7i.large")),
];
const TAGGED_VOLUME: Context = &[
    (REQUEST_COMPONENT, Literal("user-onboarding")),
    ("ec2:LaunchTemplate", LT_ARN),
];
/// The environment's tasks group, named in the template.
const TEMPLATE_GROUP: Context = &[
    ("ec2:LaunchTemplate", LT_ARN),
    ("ec2:IsLaunchTemplateResource", Literal("true")),
    (RESOURCE_COMPONENT, Literal("environment")),
];
const AMAZON_IMAGE: Context = &[
    ("ec2:Owner", Literal("amazon")),
    ("ec2:LaunchTemplate", LT_ARN),
    ("ec2:IsLaunchTemplateResource", Literal("true")),
];
/// The group pins the template to a version (`latest_version`, never
/// `$Latest`/`$Default`) and has no load balancer.
const GROUP_CREATE: Context = &[
    (REQUEST_COMPONENT, Literal("user-onboarding")),
    (
        "autoscaling:LaunchTemplateVersionSpecified",
        Literal("true"),
    ),
    ("autoscaling:InstanceTypes", Literal("m7i.large")),
];
const GROUP_UPDATE: Context = &[
    (RESOURCE_COMPONENT, Literal("user-onboarding")),
    (
        "autoscaling:LaunchTemplateVersionSpecified",
        Literal("true"),
    ),
    ("autoscaling:InstanceTypes", Literal("m7i.large")),
];
const TO_EC2: Context = &[("iam:PassedToService", Literal("ec2.amazonaws.com"))];

const EC2_COMPUTE_CALLS: &[(&str, &[Check])] = &[
    (
        "aws_ecs_cluster (user)",
        &[
            call("ecs:CreateCluster", USER_CLUSTER, USER_CREATE),
            call(
                "ecs:TagResource",
                USER_CLUSTER,
                &[("ecs:CreateAction", Literal("CreateCluster"))],
            ),
            call("ecs:DescribeClusters", USER_CLUSTER, NONE),
            call("ecs:ListTagsForResource", USER_CLUSTER, NONE),
            call("ecs:UpdateCluster", USER_CLUSTER, USER_TAGGED),
            call("ecs:DeleteCluster", USER_CLUSTER, USER_TAGGED),
        ],
    ),
    (
        // Hypothesis: authorized against the cluster and the provider.
        "aws_ecs_cluster_capacity_providers (user)",
        &[
            call("ecs:PutClusterCapacityProviders", USER_CLUSTER, USER_TAGGED),
            call(
                "ecs:PutClusterCapacityProviders",
                CAPACITY_PROVIDER,
                USER_TAGGED,
            ),
            call("ecs:DescribeClusters", USER_CLUSTER, NONE),
        ],
    ),
    (
        // Managed scaling runs as ECS's service-linked role, created on
        // first use if the account has none.
        "aws_ecs_capacity_provider",
        &[
            call("ecs:CreateCapacityProvider", CAPACITY_PROVIDER, USER_CREATE),
            call(
                "ecs:TagResource",
                CAPACITY_PROVIDER,
                &[("ecs:CreateAction", Literal("CreateCapacityProvider"))],
            ),
            call("ecs:DescribeCapacityProviders", "*", NONE),
            call("ecs:ListTagsForResource", CAPACITY_PROVIDER, NONE),
            call("ecs:UpdateCapacityProvider", CAPACITY_PROVIDER, USER_TAGGED),
            call("ecs:TagResource", CAPACITY_PROVIDER, USER_TAGGED),
            call("ecs:UntagResource", CAPACITY_PROVIDER, USER_TAGGED),
            call("ecs:DeleteCapacityProvider", CAPACITY_PROVIDER, USER_TAGGED),
            call(
                "iam:CreateServiceLinkedRole",
                ECS_SERVICE_ROLE,
                &[("iam:AWSServiceName", Literal("ecs.amazonaws.com"))],
            ),
        ],
    ),
    (
        "aws_launch_template",
        &[
            call("ec2:CreateLaunchTemplate", LAUNCH_TEMPLATE, USER_CREATE),
            call(
                "ec2:CreateTags",
                LAUNCH_TEMPLATE,
                created_by!("CreateLaunchTemplate"),
            ),
            call("ec2:DescribeLaunchTemplates", "*", NONE),
            call("ec2:DescribeLaunchTemplateVersions", "*", NONE),
            call(
                "ec2:CreateLaunchTemplateVersion",
                LAUNCH_TEMPLATE,
                USER_TAGGED,
            ),
            call("ec2:CreateTags", LAUNCH_TEMPLATE, USER_TAGGED),
            call("ec2:DeleteTags", LAUNCH_TEMPLATE, USER_TAGGED),
            call("ec2:DeleteLaunchTemplate", LAUNCH_TEMPLATE, USER_TAGGED),
        ],
    ),
    (
        // Creating the group checks that the caller could launch its
        // template (RunInstances with what the template names, the tags the
        // launch puts on, the instance role passed to EC2). The group's own
        // launches run as Auto Scaling's service-linked role. ASG tags are
        // not the provider's default_tags: the module sets them per group.
        // SetInstanceProtection: hypothesis, for draining before destroy.
        "aws_autoscaling_group",
        &[
            call(
                "autoscaling:CreateAutoScalingGroup",
                AUTO_SCALING_GROUP,
                GROUP_CREATE,
            ),
            call(
                "iam:CreateServiceLinkedRole",
                AUTO_SCALING_SERVICE_ROLE,
                &[("iam:AWSServiceName", Literal("autoscaling.amazonaws.com"))],
            ),
            call("ec2:RunInstances", LAUNCH_TEMPLATE, USER_TAGGED),
            call("ec2:RunInstances", INSTANCE, TAGGED_INSTANCE),
            call("ec2:RunInstances", VOLUME, TAGGED_VOLUME),
            call("ec2:RunInstances", ENI, FROM_TEMPLATE),
            call("ec2:RunInstances", SUBNET, FROM_TEMPLATE),
            call("ec2:RunInstances", SG, TEMPLATE_GROUP),
            call("ec2:RunInstances", IMAGE, AMAZON_IMAGE),
            call("ec2:CreateTags", INSTANCE, created_by!("RunInstances")),
            call("ec2:CreateTags", VOLUME, created_by!("RunInstances")),
            call("ec2:CreateTags", ENI, created_by!("RunInstances")),
            call("iam:PassRole", INSTANCE_ROLE, TO_EC2),
            call("autoscaling:DescribeAutoScalingGroups", "*", NONE),
            call("autoscaling:DescribeScalingActivities", "*", NONE),
            call(
                "autoscaling:UpdateAutoScalingGroup",
                AUTO_SCALING_GROUP,
                GROUP_UPDATE,
            ),
            // The drain before destroy leaves the template alone.
            call(
                "autoscaling:UpdateAutoScalingGroup",
                AUTO_SCALING_GROUP,
                USER_TAGGED,
            ),
            call(
                "autoscaling:SetInstanceProtection",
                AUTO_SCALING_GROUP,
                USER_TAGGED,
            ),
            call(
                "autoscaling:CreateOrUpdateTags",
                AUTO_SCALING_GROUP,
                USER_TAGGED,
            ),
            call("autoscaling:DeleteTags", AUTO_SCALING_GROUP, USER_TAGGED),
            call(
                "autoscaling:DeleteAutoScalingGroup",
                AUTO_SCALING_GROUP,
                USER_TAGGED,
            ),
        ],
    ),
    (
        "aws_iam_role (instance)",
        &[
            call("iam:CreateRole", INSTANCE_ROLE, BOUNDED),
            // `triple-c:cluster = <the user's cluster>`: the boundary keeps
            // the ECS agent on that cluster.
            call("iam:TagRole", INSTANCE_ROLE, NONE),
            call("iam:PutRolePolicy", INSTANCE_ROLE, BOUNDED),
            call("iam:GetRole", INSTANCE_ROLE, NONE),
            call("iam:ListInstanceProfilesForRole", INSTANCE_ROLE, NONE),
            call("iam:DeleteRolePolicy", INSTANCE_ROLE, BOUNDED),
            call("iam:DeleteRole", INSTANCE_ROLE, NONE),
        ],
    ),
    (
        // Hypothesis: adding the role needs it passable to EC2.
        "aws_iam_instance_profile",
        &[
            call("iam:CreateInstanceProfile", INSTANCE_PROFILE, NONE),
            call("iam:TagInstanceProfile", INSTANCE_PROFILE, NONE),
            call("iam:AddRoleToInstanceProfile", INSTANCE_PROFILE, NONE),
            call("iam:PassRole", INSTANCE_ROLE, TO_EC2),
            call("iam:GetInstanceProfile", INSTANCE_PROFILE, NONE),
            call("iam:ListInstanceProfileTags", INSTANCE_PROFILE, NONE),
            call("iam:UntagInstanceProfile", INSTANCE_PROFILE, NONE),
            call("iam:RemoveRoleFromInstanceProfile", INSTANCE_PROFILE, NONE),
            call("iam:DeleteInstanceProfile", INSTANCE_PROFILE, NONE),
        ],
    ),
    (
        "data.aws_ssm_parameter (AMI)",
        &[call("ssm:GetParameter", AMI_PARAMETER, NONE)],
    ),
    (
        "aws_ssm_parameter (user record)",
        &[
            call("ssm:PutParameter", USER_RECORD, USER_CREATE),
            call("ssm:AddTagsToResource", USER_RECORD, USER_CREATE),
            call("ssm:GetParameter", USER_RECORD, NONE),
            call("ssm:ListTagsForResource", USER_RECORD, NONE),
            call("ssm:DeleteParameter", USER_RECORD, NONE),
        ],
    ),
];

/// MODULE_CALLS and the phase-7 calls Task 3's resources will make.
fn all_module_calls() -> impl Iterator<Item = &'static (&'static str, &'static [Check])> {
    MODULE_CALLS.iter().chain(EC2_COMPUTE_CALLS.iter())
}

/// `resource.<type>` and `data.<type>` of the environment and onboarding
/// modules, and `backend.s3`.
fn module_types() -> BTreeSet<String> {
    let mut types = BTreeSet::new();
    for kind in [ModuleKind::Environment, ModuleKind::UserOnboard] {
        let body = parsed(kind);
        for b in top_blocks(&body, "resource") {
            types.insert(label(b, 0).unwrap().to_string());
        }
        for b in top_blocks(&body, "data") {
            types.insert(format!("data.{}", label(b, 0).unwrap()));
        }
        for t in top_blocks(&body, "terraform") {
            for backend in top_blocks(&t.body, "backend") {
                types.insert(format!("backend.{}", label(backend, 0).unwrap()));
            }
        }
    }
    types
}

#[test]
fn the_module_call_table_covers_every_type_the_modules_use() {
    let table: BTreeSet<String> = MODULE_CALLS.iter().map(|(t, _)| t.to_string()).collect();
    assert_eq!(
        table.len(),
        MODULE_CALLS.len(),
        "a type appears twice in MODULE_CALLS"
    );
    assert_eq!(module_types(), table);
}

#[test]
fn the_module_call_table_uses_the_shared_names() {
    let (task, exec) = user_role_names(ENV, OWNER_KEY);
    assert!(TASK_ROLE.ends_with(&format!("role/{task}")));
    assert!(EXEC_ROLE.ends_with(&format!("role/{exec}")));
    assert!(PARAMETER.ends_with(&format!("parameter{}", environment_parameter_name(ENV))));
    assert!(LOG_GROUP.ends_with(&format!("log-group:/triple-c/{ENV}")));
    assert!(CLUSTER.ends_with(&format!("cluster/triple-c-{ENV}")));
    assert!(ALIAS.ends_with(&format!("alias/triple-c-{ENV}")));
    let bucket = state_bucket_name("{account}", "{region}");
    assert_eq!(STATE_BUCKET, format!("arn:aws:s3:::{bucket}"));
    let env_key = state_key(Deployment::Environment(ENV));
    let user_key = state_key(Deployment::User {
        env: ENV,
        owner_key: OWNER_KEY,
    });
    assert_eq!(ENV_STATE, format!("{STATE_BUCKET}/{env_key}"));
    assert_eq!(ENV_LOCK, format!("{STATE_BUCKET}/{env_key}.tflock"));
    assert_eq!(USER_STATE, format!("{STATE_BUCKET}/{user_key}"));
    assert_eq!(USER_LOCK, format!("{STATE_BUCKET}/{user_key}.tflock"));
}

fn tier_statements(tiers: &[Tier]) -> Vec<serde_json::Value> {
    tiers
        .iter()
        .flat_map(|t| policy_files(*t).iter())
        .flat_map(|(name, json)| eval::statements_of(&eval::parse(name, json)))
        .collect()
}

fn allowed_by(stmts: &[serde_json::Value], check: &Check) -> bool {
    eval::evaluate_in(
        stmts,
        eval::Request {
            check,
            caller: eval::ME,
            owner: eval::ME,
        },
    )
}

/// Every call of every module resource is allowed by the admin tier's
/// files alone, and still allowed with the workspace tier (whose Denies an
/// admin also carries) attached as well.
#[test]
fn the_admin_tier_can_apply_every_module_resource() {
    let admin = tier_statements(&[Tier::EnvironmentAdmin]);
    let both = tier_statements(&[Tier::EnvironmentAdmin, Tier::WorkspaceUser]);
    let mut missing = Vec::new();
    for (ty, calls) in all_module_calls() {
        for c in *calls {
            for (which, stmts) in [("admin", &admin), ("admin + workspace", &both)] {
                if !allowed_by(stmts, c) {
                    missing.push(format!(
                        "{ty}: {} on {} with {:?} ({which})",
                        c.action,
                        eval::rendered(c),
                        c.context
                    ));
                }
            }
        }
    }
    assert!(missing.is_empty(), "not allowed:\n{}", missing.join("\n"));
}

/// The sample values the onboarding module's role policies are rendered
/// with: (template text, value).
const ONBOARD_SAMPLES: &[(&str, &str)] = &[
    ("${local.partition}", "aws"),
    ("${data.aws_partition.current.partition}", "aws"),
    ("${local.account}", "111122223333"),
    (
        "${data.aws_caller_identity.current.account_id}",
        "111122223333",
    ),
    ("${var.region}", "us-west-2"),
    ("${var.owner_id}", "AROAEXAMPLEME:alice@example.com"),
    ("${local.env.log_group}", "/triple-c/probe"),
    (
        "${local.env.kms_key_arn}",
        "arn:aws:kms:us-west-2:111122223333:key/00000000-0000-0000-0000-000000000000",
    ),
    (
        "${aws_efs_file_system.this.arn}",
        "arn:aws:elasticfilesystem:us-west-2:111122223333:file-system/fs-00000000",
    ),
];

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

/// Every action a per-user role's policies grant, on every resource they
/// name, is within the permissions boundary: the boundary, not the module,
/// is the ceiling, so a module change can't widen a role past it unseen.
/// Each request carries what the role's own statement requires (its
/// `StringEquals` conditions) and the component tag every Triple-C key and
/// file system has.
#[test]
fn every_onboarding_role_stays_inside_the_boundary() {
    let body = parsed(ModuleKind::UserOnboard);
    let boundary = eval::statements_of(&eval::parse(
        "permissions-boundary.json",
        permissions_boundary_json(),
    ));
    let render_sample = |text: &str| {
        let mut out = expand_locals(&body, text);
        for (from, to) in ONBOARD_SAMPLES {
            out = out.replace(from, to);
        }
        assert!(!out.contains("${"), "unrendered {out}");
        out
    };
    let mut seen = 0;
    for policy in resources(&body, "aws_iam_role_policy") {
        for stmt in statements(&jsonencoded(policy, "policy")) {
            assert_eq!(stmt["Effect"], "Allow", "{:?}", policy.labels);
            for key in stmt.as_object().unwrap().keys() {
                assert!(
                    ["Sid", "Effect", "Action", "Resource", "Condition"].contains(&key.as_str()),
                    "{:?}: {key}",
                    policy.labels
                );
            }
            let mut context: Vec<(&'static str, ContextValue)> =
                vec![(RESOURCE_COMPONENT, Literal("user-onboarding"))];
            if let Some(cond) = stmt.get("Condition") {
                for (op, entries) in cond.as_object().unwrap() {
                    assert_eq!(op, "StringEquals", "{:?}: unexpected {op}", policy.labels);
                    for (k, v) in entries.as_object().unwrap() {
                        context.push((
                            leak(k.clone()),
                            Literal(leak(render_sample(v.as_str().unwrap()))),
                        ));
                    }
                }
            }
            let context: Context = Box::leak(context.into_boxed_slice());
            for action in actions(&stmt) {
                for resource in eval::strings(&stmt["Resource"]) {
                    let check = call(
                        leak(action.clone()),
                        leak(render_sample(&resource)),
                        context,
                    );
                    assert!(
                        allowed_by(&boundary, &check),
                        "{:?}: {action} on {} with {context:?} is outside the boundary",
                        policy.labels,
                        check.resource
                    );
                    seen += 1;
                }
            }
        }
    }
    assert!(seen >= 15, "only {seen} role grants checked");
}

/// Admin write checks for calls the app makes itself, not the modules.
const APP_ONLY_WRITES: &[&str] = &[
    // The state bucket's setup (the backend only reads and writes objects).
    "s3:CreateBucket",
    "s3:TagResource",
    "s3:PutBucketVersioning",
    "s3:PutEncryptionConfiguration",
    "s3:PutBucketPublicAccessBlock",
    "s3:PutBucketTagging",
    "s3:PutBucketPolicy",
    // Workspaces (phase 5): task definitions and the roles they name
    // (`iam:PassRole` is a module call too since phase 7: the instance role).
    "ecs:RegisterTaskDefinition",
    "ecs:DeregisterTaskDefinition",
    "iam:AttachRolePolicy",
    "iam:DetachRolePolicy",
    // The permission check itself.
    "iam:SimulatePrincipalPolicy",
];

fn is_write(action: &str) -> bool {
    let verb = action.split(':').nth(1).unwrap();
    !["Describe", "List", "Get"]
        .iter()
        .any(|p| verb.starts_with(p))
}

/// The module table and the check table can't drift: every write a module
/// makes is something the permission check asks about, and every admin write
/// check is a module's call or one of the app's own.
#[test]
fn every_module_write_has_an_admin_check_and_vice_versa() {
    let checked: BTreeSet<&str> = checks(Tier::EnvironmentAdmin)
        .iter()
        .map(|c| c.action)
        .filter(|a| is_write(a))
        .collect();
    let module: BTreeSet<&str> = all_module_calls()
        .flat_map(|(_, calls)| calls.iter().map(|c| c.action))
        .filter(|a| is_write(a))
        .collect();
    let unchecked: Vec<_> = module.difference(&checked).collect();
    assert!(
        unchecked.is_empty(),
        "module writes without an admin check: {unchecked:?}"
    );
    let app_only: BTreeSet<&str> = APP_ONLY_WRITES.iter().copied().collect();
    let stray: Vec<_> = checked
        .iter()
        .filter(|a| !module.contains(*a) && !app_only.contains(*a))
        .collect();
    assert!(
        stray.is_empty(),
        "admin write checks no module or app call needs: {stray:?}"
    );
    let stale: Vec<_> = app_only
        .iter()
        .filter(|a| module.contains(*a) || !checked.contains(*a))
        .collect();
    assert!(stale.is_empty(), "APP_ONLY_WRITES is stale: {stale:?}");
}

#[test]
fn the_lifetime_cap_maximum_is_the_same_in_rust_typescript_and_hcl() {
    let max = MAX_WORKSPACE_HOURS.to_string();
    let vars = module_files(ModuleKind::Environment)
        .iter()
        .find(|(f, _)| *f == "variables.tf")
        .unwrap()
        .1;
    let block = &vars[vars.find("variable \"max_workspace_hours\"").expect("the variable")..];
    let block = &block[..block.find("\n}\n").unwrap()];
    assert!(
        block.contains(&format!("var.max_workspace_hours <= {max}")),
        "the HCL validation's maximum: {block}"
    );
    assert!(block.contains(&format!("from 0 to {max}")), "{block}");
    let ts = include_str!("../../../../src/components/settings/hosts/LifetimeCapField.tsx");
    assert!(
        ts.contains(&format!("export const MAX_WORKSPACE_HOURS = {max};")),
        "LifetimeCapField.tsx's MAX_WORKSPACE_HOURS"
    );
    // And the record parser's message.
    let envs = include_str!("../environments.rs");
    assert!(envs.contains(&format!("from 0 to {max}")));
}
