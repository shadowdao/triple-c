//! The test-only IAM evaluator the policy drift guards use (moved here from
//! `policies.rs` so the Terraform-module tests can ask it too). It is
//! deliberately small: the operators the shipped policies use and nothing
//! else; an unknown operator fails the test rather than being ignored. It is
//! not IAM -- `live_tests.rs` and the live runs are the proof.

use serde_json::Value;

use super::policies::{render, Check, ContextValue, Placeholders};

pub(crate) const AT: Placeholders = Placeholders {
    region: "us-west-2",
    account: "111122223333",
    principal: "arn:aws:iam::111122223333:role/aws-reserved/sso.amazonaws.com/us-west-2/AWSReservedSSO_Dev_0123456789abcdef",
};
pub(crate) const ME: &str = "AROAEXAMPLEME:alice@example.com";
pub(crate) const SOMEONE_ELSE: &str = "AROAEXAMPLEOTHER:bob@example.com";

/// What [`Request::resolve`] turns IAM's `${*}` into: a literal `*` in a
/// pattern, which [`glob`] matches only against a `*`.
const LITERAL_STAR: char = '\u{e000}';

/// IAM wildcard match: `*` is any run of characters (including `:` and
/// `/`), `?` is exactly one; [`LITERAL_STAR`] is a plain `*`.
pub(crate) fn glob(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while ti < t.len() {
        if pi < p.len()
            && (p[pi] == '?' || p[pi] == t[ti] || (p[pi] == LITERAL_STAR && t[ti] == '*'))
        {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

pub(crate) fn parse(name: &str, json: &str) -> Value {
    serde_json::from_str(json).unwrap_or_else(|e| panic!("{name} is not JSON: {e}"))
}

pub(crate) fn statements_of(doc: &Value) -> Vec<Value> {
    doc["Statement"]
        .as_array()
        .expect("policy has no Statement array")
        .clone()
}

pub(crate) fn effect(stmt: &Value) -> &str {
    stmt["Effect"].as_str().unwrap_or("<no Effect>")
}

/// `Action` / `Resource` / condition values: a string or an array of them.
pub(crate) fn strings(v: &Value) -> Vec<String> {
    match v {
        Value::String(s) => vec![s.clone()],
        Value::Array(a) => a
            .iter()
            .map(|x| x.as_str().expect("expected a string").to_string())
            .collect(),
        other => panic!("expected a string or array, got {other}"),
    }
}

pub(crate) fn actions(stmt: &Value) -> Vec<String> {
    strings(&stmt["Action"])
}

pub(crate) fn resources(stmt: &Value) -> Vec<String> {
    strings(&stmt["Resource"])
}

pub(crate) fn sid(stmt: &Value) -> &str {
    stmt["Sid"].as_str().unwrap_or("<no Sid>")
}

pub(crate) fn rendered(check: &Check) -> String {
    render(check.resource, &AT)
}

/// The request: who calls, and whose the resource / request tag is.
#[derive(Clone, Copy)]
pub(crate) struct Request<'a> {
    pub check: &'a Check,
    /// `${aws:userid}` of the caller.
    pub caller: &'a str,
    /// What `OwnerId` context entries carry.
    pub owner: &'a str,
}

impl Request<'_> {
    pub(crate) fn context_value(&self, key: &str) -> Option<String> {
        self.context_values(key).into_iter().next()
    }

    /// All values of a (possibly multi-valued) key.
    pub(crate) fn context_values(&self, key: &str) -> Vec<String> {
        self.check
            .context
            .iter()
            .filter(|(k, _)| *k == key)
            .map(|(_, v)| match v {
                ContextValue::OwnerId => self.owner.to_string(),
                ContextValue::Literal(l) => render(l, &AT),
            })
            .collect()
    }

    /// Resolves `${…}` policy variables the way the simulator does: from
    /// the request context. An unresolvable variable makes the string
    /// match nothing (`None`).
    pub(crate) fn resolve(&self, s: &str) -> Option<String> {
        let mut out = String::new();
        let mut rest = s;
        while let Some(start) = rest.find("${") {
            out.push_str(&rest[..start]);
            let end = rest[start..].find('}').expect("unterminated ${") + start;
            let name = &rest[start + 2..end];
            let value = if name == "*" {
                Some(LITERAL_STAR.to_string())
            } else if name == "aws:userid" {
                Some(self.caller.to_string())
            } else {
                self.context_value(name)
            };
            out.push_str(&value?);
            rest = &rest[end + 1..];
        }
        out.push_str(rest);
        Some(out)
    }
}

/// Statements that name the action and one of whose resources matches
/// the rendered resource.
pub(crate) fn matching<'s>(stmts: &'s [Value], req: &Request) -> Vec<&'s Value> {
    let resource = rendered(req.check);
    stmts
        .iter()
        .filter(|s| actions(s).iter().any(|a| glob(a, req.check.action)))
        .filter(|s| {
            resources(s)
                .iter()
                .filter_map(|r| req.resolve(r))
                .any(|r| glob(&r, &resource))
        })
        .collect()
}

/// A deliberately small IAM condition evaluator: the operators the
/// shipped policies use, and nothing else (an unknown operator fails the
/// test rather than being ignored).
pub(crate) fn conditions_hold(stmt: &Value, req: &Request) -> bool {
    let Some(cond) = stmt.get("Condition") else {
        return true;
    };
    let cond = cond.as_object().expect("Condition is an object");
    cond.iter().all(|(op, entries)| {
        let entries = entries.as_object().expect("condition block is an object");
        // ForAllValues: every value of the (multi-valued) key matches
        // one of the policy's values. Vacuously true when the key is
        // absent, which is why statements pair it with a required key.
        if let Some(base) = op.strip_prefix("ForAllValues:") {
            assert_eq!(
                base,
                "StringEquals",
                "{}: unsupported ForAllValues operator",
                sid(stmt)
            );
            return entries.iter().all(|(key, want)| {
                let allowed: Vec<String> = strings(want)
                    .iter()
                    .filter_map(|p| req.resolve(p))
                    .collect();
                req.context_values(key).iter().all(|v| allowed.contains(v))
            });
        }
        if op == "Null" {
            return entries.iter().all(|(key, want)| {
                let absent = req.context_value(key).is_none();
                match want.as_str() {
                    Some("true") => absent,
                    Some("false") => !absent,
                    _ => panic!("{}: Null takes \"true\" or \"false\"", sid(stmt)),
                }
            });
        }
        let (base, if_exists) = match op.strip_suffix("IfExists") {
            Some(b) => (b, true),
            None => (op.as_str(), false),
        };
        // A negated operator holds when the key is absent, and
        // otherwise when no value matches.
        let (matches, negated): (fn(&str, &str) -> bool, bool) = match base {
            "StringEquals" => (|p, v| p == v, false),
            "StringLike" | "ArnLike" => (glob, false),
            "StringNotEquals" => (|p, v| p == v, true),
            "StringNotLike" => (glob, true),
            other => panic!("{}: unsupported condition operator {other}", sid(stmt)),
        };
        entries
            .iter()
            .all(|(key, want)| match req.context_value(key) {
                None => if_exists || negated,
                Some(value) => {
                    let any = strings(want)
                        .iter()
                        .filter_map(|p| req.resolve(p))
                        .any(|p| matches(&p, &value));
                    any != negated
                }
            })
    })
}

/// Allowed by some Allow and by no Deny among `stmts`.
pub(crate) fn evaluate_in(stmts: &[Value], req: Request) -> bool {
    let applies = |s: &&Value| conditions_hold(s, &req);
    let allowed = matching(stmts, &req)
        .into_iter()
        .filter(|s| effect(s) == "Allow")
        .any(|s| applies(&s));
    let denied = matching(stmts, &req)
        .into_iter()
        .filter(|s| effect(s) == "Deny")
        .any(|s| applies(&s));
    allowed && !denied
}

#[cfg(test)]
mod tests {
    use super::*;
    use ContextValue::Literal;

    #[test]
    fn glob_behaves_like_iam_wildcards() {
        assert!(glob("ecs:Describe*", "ecs:DescribeClusters"));
        assert!(!glob("ecs:Describe*", "ecs:ListClusters"));
        assert!(glob(
            "arn:aws:ecs:*:*:task/triple-c-*/*",
            "arn:aws:ecs:us-west-2:1:task/triple-c-x/abc"
        ));
        assert!(!glob(
            "arn:aws:ecs:*:*:task/triple-c-*/*",
            "arn:aws:ecs:us-west-2:1:task/other/abc"
        ));
        assert!(glob("a?c", "abc"));
        assert!(!glob("a?c", "ac"));
        // `${*}` (resolved to LITERAL_STAR) matches only a real `*`.
        let literal = format!("arn:aws:ec2:*:*:{LITERAL_STAR}/{LITERAL_STAR}");
        assert!(glob(&literal, "arn:aws:ec2:us-west-2:1:*/*"));
        assert!(!glob(&literal, "arn:aws:ec2:us-west-2:1:elastic-ip/eipalloc-1"));
    }

    #[test]
    fn for_all_values_is_vacuous_on_an_absent_key() {
        let stmt: Value = serde_json::json!({
            "Sid": "T", "Effect": "Allow", "Action": "x:Y", "Resource": "*",
            "Condition": { "ForAllValues:StringEquals": { "aws:TagKeys": ["a", "b"] } }
        });
        let check = |context| Check {
            action: "x:Y",
            resource: "*",
            context,
            write: true,
        };
        let holds = |c: &Check| {
            conditions_hold(
                &stmt,
                &Request {
                    check: c,
                    caller: ME,
                    owner: ME,
                },
            )
        };
        assert!(holds(&check(&[])));
        assert!(holds(&check(&[
            ("aws:TagKeys", Literal("a")),
            ("aws:TagKeys", Literal("b"))
        ])));
        assert!(!holds(&check(&[
            ("aws:TagKeys", Literal("a")),
            ("aws:TagKeys", Literal("c"))
        ])));
    }
}
