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
        state_key(ModuleKind::Environment, Some("dev"), None),
        "environments/dev/terraform.tfstate"
    );
    assert_eq!(
        state_key(ModuleKind::UserOnboard, Some("dev"), Some("0123456789ab")),
        "users/dev/0123456789ab/terraform.tfstate"
    );
    assert_eq!(
        state_key(ModuleKind::PermissionsBoundary, None, None),
        "boundary/terraform.tfstate"
    );
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

#[test]
fn no_provider_endpoint_overrides() {
    let body = parsed(ModuleKind::Environment);
    let provider = aws_provider(&body);
    assert_eq!(
        render(attr(&provider.body, "region").unwrap()),
        "var.region"
    );
    assert!(attr(&provider.body, "endpoints").is_none());
    assert!(top_blocks(&provider.body, "endpoints").next().is_none());
    let text = tf_text(ModuleKind::Environment);
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
    let lock = module_files(ModuleKind::Environment)
        .iter()
        .find(|(name, _)| *name == ".terraform.lock.hcl")
        .expect("lock file shipped")
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
    let names: Vec<_> = module_files(ModuleKind::Environment)
        .iter()
        .map(|(n, _)| *n)
        .collect();
    for want in [
        "versions.tf",
        "variables.tf",
        "main.tf",
        "network.tf",
        "outputs.tf",
        ".terraform.lock.hcl",
    ] {
        assert!(names.contains(&want), "{want} missing from {names:?}");
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
