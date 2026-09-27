//! Builds the tar a project's container receives: every effective install's
//! files, read from the cache at its pinned commit, plus `manifest.json` and a
//! generated Claude Code catalog per marketplace that contributes plugins.
//! Layout: see the Interface Contract in the plan / spec §4. The tar carries
//! no directory entries — the sync script's extraction (plus its umask)
//! creates them.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::{json, Value};

use super::catalog::{item_files, plugin_catalog_entry, rendered_hook_settings, ItemFile};
use super::git;
use super::tree::GitTree;
use crate::models::marketplace::{
    is_valid_commit, is_valid_item_key, marketplace_slug, ItemKind, Marketplace,
    MarketplaceInstall, SkippedItem,
};

pub struct PayloadInput<'a> {
    pub installs: &'a [MarketplaceInstall],
    pub marketplaces: &'a [Marketplace],
    /// data root used to find caches (see git::cache_path)
    pub data_root: &'a Path,
}

pub struct Payload {
    pub tar: Vec<u8>,
    pub manifest: Value,
    pub skipped: Vec<SkippedItem>,
}

/// A relative path from `item_files` is joined under a directory we chose, so
/// it must not be able to climb out of it. The catalog already refuses such
/// entries; this is the second line.
fn safe_rel(rel: &str) -> bool {
    !rel.is_empty()
        && !rel.starts_with('/')
        && !rel.contains('\\')
        && rel
            .split('/')
            .all(|seg| !seg.is_empty() && seg != "." && seg != "..")
}

struct TarWriter {
    builder: tar::Builder<Vec<u8>>,
    mtime: u64,
}

impl TarWriter {
    fn new() -> Self {
        let mtime = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Self {
            builder: tar::Builder::new(Vec::new()),
            mtime,
        }
    }

    fn file(&mut self, path: &str, data: &[u8], executable: bool) -> Result<(), String> {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(if executable { 0o755 } else { 0o644 });
        header.set_mtime(self.mtime);
        header.set_entry_type(tar::EntryType::Regular);
        self.builder
            .append_data(&mut header, path, data)
            .map_err(|e| format!("Could not add {path} to the marketplace payload: {e}"))
    }

    fn finish(self) -> Result<Vec<u8>, String> {
        self.builder
            .into_inner()
            .map_err(|e| format!("Could not finish the marketplace payload: {e}"))
    }
}

struct PluginGroup {
    entries: Vec<Value>,
    keys: Vec<String>,
}

/// Files of one install, validated for use as payload paths.
fn install_files(
    repo: &Path,
    inst: &MarketplaceInstall,
) -> Result<(GitTree, Vec<ItemFile>), String> {
    let tree = GitTree::open(repo, &inst.commit)?;
    let files = item_files(&tree, inst.kind, &inst.key)?;
    if let Some(bad) = files.iter().find(|f| !safe_rel(&f.rel_path)) {
        return Err(format!("contains an unsafe path ({})", bad.rel_path));
    }
    Ok((tree, files))
}

pub fn build_payload(input: &PayloadInput) -> Result<Payload, String> {
    let mut tar = TarWriter::new();
    let mut items: Vec<Value> = Vec::new();
    let mut skipped: Vec<SkippedItem> = Vec::new();
    // State ids (see sync.sh) of installs the host could not build this time:
    // the container keeps what it has for them instead of treating them as
    // deselected (final review M3). Only a removed source really removes.
    let mut held: BTreeSet<String> = BTreeSet::new();
    let mut plugin_groups: BTreeMap<String, PluginGroup> = BTreeMap::new();
    // Non-plugin items share one namespace in ~/.claude; plugins are namespaced
    // by their per-marketplace catalog, so they never collide.
    let mut taken: BTreeSet<(ItemKind, String)> = BTreeSet::new();

    for inst in input.installs {
        let label = format!("{}:{}", inst.kind.as_str(), inst.key);
        let mut skip = |reason: String| {
            skipped.push(SkippedItem {
                item: label.clone(),
                reason,
            })
        };

        let Some(m) = input
            .marketplaces
            .iter()
            .find(|m| m.id == inst.marketplace_id)
        else {
            skip("its marketplace has been removed".to_string());
            continue;
        };
        let state_id = match inst.kind {
            ItemKind::Plugin => format!("plugin:{}/{}", marketplace_slug(&m.id), inst.key),
            _ => label.clone(),
        };
        let mut hold = |reason: String| {
            held.insert(state_id.clone());
            skip(reason)
        };
        if !is_valid_item_key(&inst.key) {
            skip("the saved install entry is invalid".to_string());
            continue;
        }
        if !is_valid_commit(&inst.commit) {
            hold("the saved install entry is invalid".to_string());
            continue;
        }
        if inst.kind != ItemKind::Plugin && taken.contains(&(inst.kind, inst.key.clone())) {
            skip(format!(
                "another marketplace's {label} is already installed"
            ));
            continue;
        }
        let repo = git::cache_path(input.data_root, &m.id);
        if !git::has_commit(&repo, &inst.commit) {
            hold(format!(
                "pinned commit {} is not in the local cache of \"{}\" — refresh the marketplace",
                &inst.commit[..8],
                m.name
            ));
            continue;
        }
        let (tree, files) = match install_files(&repo, inst) {
            Ok(v) => v,
            Err(e) => {
                hold(e);
                continue;
            }
        };

        let key = &inst.key;
        let mut item = json!({
            "kind": inst.kind.as_str(),
            "key": key,
            "marketplace": m.id,
            "commit": inst.commit,
        });
        match inst.kind {
            ItemKind::Agent | ItemKind::Command => {
                let dir = if inst.kind == ItemKind::Agent {
                    "agents"
                } else {
                    "commands"
                };
                let Some(f) = files.first() else {
                    hold("has no files".to_string());
                    continue;
                };
                let path = format!("{dir}/{key}.md");
                tar.file(&path, &f.data, false)?;
                item["file"] = json!(path);
            }
            ItemKind::Skill | ItemKind::Hook => {
                let dir = if inst.kind == ItemKind::Skill {
                    format!("skills/{key}")
                } else {
                    format!("hooks/{key}")
                };
                if inst.kind == ItemKind::Hook {
                    match rendered_hook_settings(&tree, key) {
                        Ok(settings) => item["settings"] = settings,
                        Err(e) => {
                            hold(e);
                            continue;
                        }
                    }
                }
                for f in &files {
                    tar.file(&format!("{dir}/{}", f.rel_path), &f.data, f.executable)?;
                }
                item["dir"] = json!(dir);
            }
            ItemKind::Plugin => {
                let mut entry = match plugin_catalog_entry(&tree, key) {
                    Ok(e) => e,
                    Err(e) => {
                        hold(e);
                        continue;
                    }
                };
                entry["source"] = json!(format!("./{key}"));
                let slug = marketplace_slug(&m.id);
                for f in &files {
                    tar.file(
                        &format!("plugins/{slug}/{key}/{}", f.rel_path),
                        &f.data,
                        f.executable,
                    )?;
                }
                let group = plugin_groups
                    .entry(slug.clone())
                    .or_insert_with(|| PluginGroup {
                        entries: Vec::new(),
                        keys: Vec::new(),
                    });
                group.entries.push(entry);
                group.keys.push(key.clone());
                item["slug"] = json!(slug);
            }
        }
        if inst.kind != ItemKind::Plugin {
            taken.insert((inst.kind, key.clone()));
        }
        items.push(item);
    }

    let mut plugin_marketplaces = Vec::new();
    for (slug, group) in plugin_groups {
        let catalog = json!({
            "name": format!("triple-c-{slug}"),
            "owner": { "name": "Triple-C" },
            "plugins": group.entries,
        });
        let bytes = serde_json::to_vec_pretty(&catalog).map_err(|e| e.to_string())?;
        tar.file(
            &format!("plugins/{slug}/.claude-plugin/marketplace.json"),
            &bytes,
            false,
        )?;
        plugin_marketplaces
            .push(json!({ "slug": slug, "dir": format!("plugins/{slug}"), "plugins": group.keys }));
    }

    let manifest = json!({
        "version": 1,
        "items": items,
        "plugin_marketplaces": plugin_marketplaces,
        "held": held,
    });
    let bytes = serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?;
    tar.file("manifest.json", &bytes, false)?;

    Ok(Payload {
        tar: tar.finish()?,
        manifest,
        skipped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::marketplace::test_support::GitFixture;
    use std::collections::HashMap;
    use std::io::Read;

    struct Entry {
        data: Vec<u8>,
        mode: u32,
    }

    fn unpack(tar_bytes: &[u8]) -> HashMap<String, Entry> {
        let mut archive = tar::Archive::new(tar_bytes);
        let mut out = HashMap::new();
        for e in archive.entries().unwrap() {
            let mut e = e.unwrap();
            let path = e.path().unwrap().to_string_lossy().into_owned();
            let mode = e.header().mode().unwrap();
            let mut data = Vec::new();
            e.read_to_end(&mut data).unwrap();
            out.insert(path, Entry { data, mode });
        }
        out
    }

    fn market(id: &str) -> Marketplace {
        Marketplace {
            id: id.into(),
            name: "Team Tools".into(),
            url: "https://example.invalid/r.git".into(),
            branch: None,
            account_id: None,
        }
    }

    fn inst(kind: ItemKind, key: &str, commit: &str) -> MarketplaceInstall {
        MarketplaceInstall {
            marketplace_id: "m1aaaaaaaa".into(),
            kind,
            key: key.into(),
            commit: commit.into(),
        }
    }

    /// Fetch the fixture into `<data>/marketplaces/m1aaaaaaaa.git`.
    fn cache(fx: &GitFixture, data: &Path) {
        let repo = git::cache_path(data, "m1aaaaaaaa");
        git::fetch(&repo, &fx.url(), None, None).unwrap();
    }

    #[test]
    fn every_kind_lands_at_its_contract_path() {
        let Some(fx) = GitFixture::new() else { return };
        let c = fx.with_all_kinds();
        let data = tempfile::tempdir().unwrap();
        cache(&fx, data.path());
        let installs = vec![
            inst(ItemKind::Agent, "code-reviewer", &c),
            inst(ItemKind::Skill, "example-skill", &c),
            inst(ItemKind::Command, "example-command", &c),
            inst(ItemKind::Hook, "notify-on-stop", &c),
            inst(ItemKind::Plugin, "example-plugin", &c),
        ];
        let marketplaces = vec![market("m1aaaaaaaa")];
        let p = build_payload(&PayloadInput {
            installs: &installs,
            marketplaces: &marketplaces,
            data_root: data.path(),
        })
        .unwrap();

        assert!(p.skipped.is_empty(), "{:?}", p.skipped);
        let files = unpack(&p.tar);
        let slug = marketplace_slug("m1aaaaaaaa");
        for path in [
            "agents/code-reviewer.md".to_string(),
            "skills/example-skill/SKILL.md".to_string(),
            "commands/example-command.md".to_string(),
            "hooks/notify-on-stop/hook.json".to_string(),
            "hooks/notify-on-stop/notify.sh".to_string(),
            format!("plugins/{slug}/.claude-plugin/marketplace.json"),
            format!("plugins/{slug}/example-plugin/.claude-plugin/plugin.json"),
            format!("plugins/{slug}/example-plugin/skills/hello/SKILL.md"),
            "manifest.json".to_string(),
        ] {
            assert!(
                files.contains_key(&path),
                "missing {path}; have {:?}",
                files.keys().collect::<Vec<_>>()
            );
        }
        assert_eq!(files["hooks/notify-on-stop/notify.sh"].mode & 0o777, 0o755);
        assert_eq!(files["agents/code-reviewer.md"].mode & 0o777, 0o644);
    }

    #[test]
    fn manifest_and_generated_catalog_match_the_contract() {
        let Some(fx) = GitFixture::new() else { return };
        let c = fx.with_all_kinds();
        let data = tempfile::tempdir().unwrap();
        cache(&fx, data.path());
        let installs = vec![
            inst(ItemKind::Hook, "notify-on-stop", &c),
            inst(ItemKind::Plugin, "example-plugin", &c),
        ];
        let marketplaces = vec![market("m1aaaaaaaa")];
        let p = build_payload(&PayloadInput {
            installs: &installs,
            marketplaces: &marketplaces,
            data_root: data.path(),
        })
        .unwrap();
        let slug = marketplace_slug("m1aaaaaaaa");

        let files = unpack(&p.tar);
        let manifest: Value = serde_json::from_slice(&files["manifest.json"].data).unwrap();
        assert_eq!(manifest, p.manifest);
        assert_eq!(manifest["version"], 1);
        let hook = &manifest["items"][0];
        assert_eq!(hook["kind"], "hook");
        assert_eq!(hook["dir"], "hooks/notify-on-stop");
        assert_eq!(
            hook["settings"]["Stop"][0]["hooks"][0]["command"],
            "/home/claude/.claude/triple-c/hooks/notify-on-stop/notify.sh"
        );
        let plugin = &manifest["items"][1];
        assert_eq!(plugin["kind"], "plugin");
        assert_eq!(plugin["slug"], slug.as_str());
        assert_eq!(
            manifest["plugin_marketplaces"],
            json!([{ "slug": slug, "dir": format!("plugins/{slug}"), "plugins": ["example-plugin"] }])
        );

        let catalog: Value = serde_json::from_slice(
            &files[&format!("plugins/{slug}/.claude-plugin/marketplace.json")].data,
        )
        .unwrap();
        assert_eq!(catalog["name"], format!("triple-c-{slug}"));
        assert_eq!(catalog["owner"]["name"], "Triple-C");
        assert_eq!(catalog["plugins"][0]["name"], "example-plugin");
        assert_eq!(catalog["plugins"][0]["source"], "./example-plugin");
    }

    /// Final review M4: the plugin marketplace name comes from the id, so a
    /// rename never makes the container see a different marketplace.
    #[test]
    fn plugin_slug_survives_a_rename() {
        let Some(fx) = GitFixture::new() else { return };
        let c = fx.with_all_kinds();
        let data = tempfile::tempdir().unwrap();
        cache(&fx, data.path());
        let installs = vec![inst(ItemKind::Plugin, "example-plugin", &c)];
        let slug_named = |name: &str| {
            let marketplaces = vec![Marketplace {
                name: name.into(),
                ..market("m1aaaaaaaa")
            }];
            let p = build_payload(&PayloadInput {
                installs: &installs,
                marketplaces: &marketplaces,
                data_root: data.path(),
            })
            .unwrap();
            p.manifest["items"][0]["slug"].as_str().unwrap().to_string()
        };
        assert_eq!(slug_named("Team Tools"), "mp-m1aaaaaa");
        assert_eq!(slug_named("Renamed"), "mp-m1aaaaaa");
    }

    #[test]
    fn items_that_cannot_be_built_are_skipped_not_fatal() {
        let Some(fx) = GitFixture::new() else { return };
        let c = fx.with_all_kinds();
        let data = tempfile::tempdir().unwrap();
        cache(&fx, data.path());
        let mut gone = inst(ItemKind::Agent, "code-reviewer", &c);
        gone.marketplace_id = "removed".into();
        let installs = vec![
            gone,
            inst(ItemKind::Agent, "code-reviewer", &"0".repeat(40)),
            inst(ItemKind::Agent, "does-not-exist", &c),
            inst(ItemKind::Command, "example-command", &c),
        ];
        let marketplaces = vec![market("m1aaaaaaaa")];
        let p = build_payload(&PayloadInput {
            installs: &installs,
            marketplaces: &marketplaces,
            data_root: data.path(),
        })
        .unwrap();

        let skipped: Vec<&str> = p.skipped.iter().map(|s| s.item.as_str()).collect();
        assert_eq!(
            skipped,
            vec![
                "agent:code-reviewer",
                "agent:code-reviewer",
                "agent:does-not-exist"
            ]
        );
        assert!(
            p.skipped[0].reason.contains("marketplace"),
            "{}",
            p.skipped[0].reason
        );
        assert!(
            p.skipped[1].reason.contains("cache"),
            "{}",
            p.skipped[1].reason
        );
        assert_eq!(p.manifest["items"].as_array().unwrap().len(), 1);
        // Final review M3: host-side failures are held (the container keeps
        // what it has); only a removed source really removes.
        assert_eq!(
            p.manifest["held"],
            json!(["agent:code-reviewer", "agent:does-not-exist"])
        );
    }

    #[test]
    fn a_plugin_that_cannot_be_built_is_held_under_its_marketplace() {
        let Some(fx) = GitFixture::new() else { return };
        fx.with_all_kinds();
        let data = tempfile::tempdir().unwrap();
        cache(&fx, data.path());
        let installs = vec![inst(ItemKind::Plugin, "example-plugin", &"0".repeat(40))];
        let marketplaces = vec![market("m1aaaaaaaa")];
        let p = build_payload(&PayloadInput {
            installs: &installs,
            marketplaces: &marketplaces,
            data_root: data.path(),
        })
        .unwrap();
        assert_eq!(p.skipped.len(), 1);
        assert_eq!(
            p.manifest["held"],
            json!([format!(
                "plugin:{}/example-plugin",
                marketplace_slug("m1aaaaaaaa")
            )])
        );
        assert_eq!(p.manifest["plugin_marketplaces"], json!([]));
    }

    #[test]
    fn a_second_marketplace_cannot_shadow_an_installed_name() {
        let Some(fx) = GitFixture::new() else { return };
        let c = fx.with_all_kinds();
        let data = tempfile::tempdir().unwrap();
        cache(&fx, data.path());
        let other = git::cache_path(data.path(), "m2bbbbbbbb");
        git::fetch(&other, &fx.url(), None, None).unwrap();
        let mut second = inst(ItemKind::Agent, "code-reviewer", &c);
        second.marketplace_id = "m2bbbbbbbb".into();
        let installs = vec![inst(ItemKind::Agent, "code-reviewer", &c), second];
        let marketplaces = vec![market("m1aaaaaaaa"), market("m2bbbbbbbb")];
        let p = build_payload(&PayloadInput {
            installs: &installs,
            marketplaces: &marketplaces,
            data_root: data.path(),
        })
        .unwrap();
        assert_eq!(p.manifest["items"].as_array().unwrap().len(), 1);
        assert_eq!(p.skipped.len(), 1);
        assert!(p.skipped[0].reason.contains("another marketplace"));
        assert_eq!(p.manifest["held"], json!([]));
    }

    #[test]
    fn an_empty_install_set_still_yields_a_manifest() {
        let data = tempfile::tempdir().unwrap();
        let p = build_payload(&PayloadInput {
            installs: &[],
            marketplaces: &[],
            data_root: data.path(),
        })
        .unwrap();
        assert_eq!(
            p.manifest,
            json!({ "version": 1, "items": [], "plugin_marketplaces": [], "held": [] })
        );
        assert!(unpack(&p.tar).contains_key("manifest.json"));
    }
}
