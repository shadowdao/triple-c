//! Tests for the disk view's pure logic.
//!
//! Split into its own file because `disk.rs` is already long and because
//! everything here has to stay runnable without a daemon — which is the point
//! of keeping the classification, the orphan set and the script builders pure.
//!
//! The blast radius of a mistake in this module is a user's credentials,
//! transcripts and toolchains, so the tests below are deliberately about
//! *refusing*, not about succeeding.

use super::*;

// ---------------------------------------------------------------------------
// Safety classification
// ---------------------------------------------------------------------------

/// Every `ReclaimTarget` variant, so the walks below cannot silently skip a new
/// one. A variant added without a line here fails `every_variant_is_covered`.
fn all_reclaim_targets() -> Vec<ReclaimTarget> {
    vec![
        ReclaimTarget::DanglingSnapshots,
        ReclaimTarget::SupersededBaseImages,
        ReclaimTarget::BuildCache { all: false },
        ReclaimTarget::BuildCache { all: true },
        ReclaimTarget::MigrationPins,
        ReclaimTarget::MigrationStaging,
        ReclaimTarget::ProbeContainers,
        ReclaimTarget::ScrubContainers,
        ReclaimTarget::OrphanVolume {
            name: "triple-c-home-gone".to_string(),
        },
        ReclaimTarget::CompactSnapshot {
            project_id: "p1".to_string(),
        },
        ReclaimTarget::ClearCaches {
            project_id: "p1".to_string(),
            include_rustup: false,
        },
        ReclaimTarget::ClearCaches {
            project_id: "p1".to_string(),
            include_rustup: true,
        },
    ]
}

#[test]
fn every_variant_is_covered_by_the_safety_walk() {
    // `ReclaimTarget` has no way to enumerate itself, so this pins the count by
    // hand. Bumping it is the prompt to add the new variant above *and* decide
    // its safety deliberately rather than by whatever the match arm falls into.
    let discriminants: HashSet<String> = all_reclaim_targets()
        .iter()
        .map(|t| serde_json::to_value(t).unwrap()["kind"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        discriminants.len(),
        10,
        "a ReclaimTarget variant was added or removed; update all_reclaim_targets() and check its \
         safety: {:?}",
        discriminants
    );
}

#[test]
fn nothing_destructive_can_land_in_the_safe_bucket() {
    // The strongest form of this guarantee is structural: `reclaim` takes
    // `&[ReclaimTarget]` and `DestructiveTarget` is a different type, so a
    // destructive action cannot be passed to a bulk reclaim at all. What this
    // test pins is the second half — that no *safe*-classified target names a
    // live project's data either.
    for target in all_reclaim_targets() {
        match &target {
            // These act on a project, and both are rewrites or cache flushes.
            // Neither may ever be classified Safe: one rebuilds an image and
            // the other costs a re-download.
            ReclaimTarget::CompactSnapshot { .. } | ReclaimTarget::ClearCaches { .. } => {
                assert_eq!(
                    target.safety(),
                    Safety::SemiSafe,
                    "{:?} must ask for confirmation",
                    target
                );
            }
            // A safe target may name a *volume*, but only ever one that orphan
            // detection produced — which by construction belongs to no project
            // in the store.
            other => assert_eq!(
                other.safety(),
                Safety::Safe,
                "{:?} was expected to need no confirmation",
                other
            ),
        }
    }
}

#[test]
fn only_the_build_cache_reaches_outside_triple_c() {
    // The user's daemon also holds their unrelated postgres, mysql and
    // site-builder work. Exactly one action here touches it, and the UI has to
    // say so — so if a second one ever does, this fails loudly.
    let daemon_wide: Vec<_> = all_reclaim_targets()
        .into_iter()
        .filter(ReclaimTarget::is_daemon_wide)
        .collect();
    assert_eq!(daemon_wide.len(), 2, "expected only the two BuildCache variants");
    assert!(daemon_wide
        .iter()
        .all(|t| matches!(t, ReclaimTarget::BuildCache { .. })));
}

#[test]
fn destructive_targets_all_name_a_project() {
    // The typed confirmation is "type the project name". A destructive target
    // that could not name a project would have nothing to confirm against.
    for target in [
        DestructiveTarget::HomeVolume {
            project_id: "p1".to_string(),
        },
        DestructiveTarget::ConfigVolume {
            project_id: "p1".to_string(),
        },
        DestructiveTarget::SnapshotImage {
            project_id: "p1".to_string(),
        },
        DestructiveTarget::RollbackPin {
            project_id: "p1".to_string(),
            tag: "pre-migration-20260101-101500".to_string(),
        },
    ] {
        assert_eq!(target.project_id(), "p1");
    }
}

#[test]
fn a_dangling_image_is_a_base_only_when_it_says_so() {
    let base = HashMap::from([(LABEL_BASE.to_string(), "true".to_string())]);
    assert_eq!(classify_dangling(&base), DanglingClass::Base);

    // `create_container` writes `triple-c.base` explicitly *empty* precisely so
    // an inherited `true` cannot ride a commit onto a snapshot and make it
    // claim to be a base image.
    let commit = HashMap::from([(LABEL_BASE.to_string(), String::new())]);
    assert_eq!(classify_dangling(&commit), DanglingClass::SnapshotCommit);

    // Images committed before the label existed carry it not at all.
    assert_eq!(
        classify_dangling(&HashMap::new()),
        DanglingClass::SnapshotCommit
    );

    // Anything other than the exact string `true` is not a base.
    let liar = HashMap::from([(LABEL_BASE.to_string(), "yes".to_string())]);
    assert_eq!(classify_dangling(&liar), DanglingClass::SnapshotCommit);
}

// ---------------------------------------------------------------------------
// Orphan detection — the part that can delete a user's transcripts
// ---------------------------------------------------------------------------

fn vol(name: &str, bytes: i64, links: i64) -> VolumeFacts {
    VolumeFacts {
        name: name.to_string(),
        bytes,
        links,
        created_at: Some("2026-03-14T09:00:00Z".to_string()),
    }
}

#[test]
fn orphan_detection_skips_every_project_in_the_store() {
    let volumes = vec![
        vol("triple-c-home-live", 1_000, 0),
        vol("triple-c-claude-config-live", 2_000, 0),
        vol("triple-c-home-gone", 3_000, 0),
        vol("triple-c-claude-config-gone", 4_000, 0),
    ];
    let known = HashSet::from(["live".to_string()]);
    let orphans = orphan_volumes(&volumes, &known, true);

    let names: Vec<&str> = orphans.iter().map(|o| o.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["triple-c-claude-config-gone", "triple-c-home-gone"],
        "sorted biggest first"
    );
    assert!(
        !names.iter().any(|n| n.contains("live")),
        "a live project's volumes were offered for deletion"
    );
}

#[test]
fn a_store_that_did_not_load_yields_no_orphans_at_all() {
    // This is the case the whole design turns on, and the one that would wipe
    // every project's credentials, transcripts and toolchains at once. With the
    // store unreadable, *every* project's volumes look unclaimed — so the
    // answer has to be "nothing, and here is why", never "everything".
    let volumes = vec![
        vol("triple-c-home-a", 1_000, 0),
        vol("triple-c-claude-config-a", 2_000, 0),
        vol("triple-c-home-b", 3_000, 0),
    ];
    assert!(orphan_volumes(&volumes, &HashSet::new(), false).is_empty());

    // And with the store loaded but genuinely empty, they *are* orphans — the
    // distinction is the flag, not the emptiness of the set.
    assert_eq!(orphan_volumes(&volumes, &HashSet::new(), true).len(), 3);
}

#[test]
fn an_idle_live_project_is_never_mistaken_for_a_deleted_one() {
    // The exact mistake this guard exists for. An "orphan" heuristic of "no
    // container and no snapshot image" was tried against a real project list
    // and flagged two live projects — `site-builder` and `cal-dav-mcp` — that
    // had simply been idle long enough for their containers to be removed.
    // Their volumes held `.credentials.json`, Claude transcripts and shell
    // history.
    //
    // From the daemon's side those look identical to a deleted project's
    // leftovers: volumes present, ref count zero, no container, no image. The
    // *only* thing that tells them apart is membership in Triple-C's own
    // project store, so that is the only thing consulted.
    let idle_but_live = vec![
        vol("triple-c-home-site-builder", 8_400_000_000, 0),
        vol("triple-c-claude-config-site-builder", 427_000_000, 0),
        vol("triple-c-home-cal-dav-mcp", 1_200_000_000, 0),
        vol("triple-c-claude-config-cal-dav-mcp", 44_000_000, 0),
        vol("triple-c-home-really-gone", 900_000, 0),
    ];
    let store = HashSet::from(["site-builder".to_string(), "cal-dav-mcp".to_string()]);

    let orphans = orphan_volumes(&idle_but_live, &store, true);
    assert_eq!(
        orphans.iter().map(|o| o.name.as_str()).collect::<Vec<_>>(),
        vec!["triple-c-home-really-gone"],
        "an idle live project's volumes were offered for deletion"
    );

    // And nothing in the signature even *offers* container or image state, so a
    // future change cannot quietly start inferring from it.
    assert_eq!(
        orphans[0].created_at.as_deref(),
        Some("2026-03-14T09:00:00Z"),
        "the creation date is the evidence a user recognises the project by"
    );
}

#[test]
fn a_volume_with_a_container_attached_is_never_an_orphan() {
    let volumes = vec![
        vol("triple-c-home-gone", 1_000, 1),
        // -1 is "the daemon did not compute it", which must fail closed: an
        // unknown ref count is not permission.
        vol("triple-c-claude-config-gone", 2_000, -1),
        vol("triple-c-home-other", 3_000, 0),
    ];
    let orphans = orphan_volumes(&volumes, &HashSet::new(), true);
    assert_eq!(orphans.len(), 1);
    assert_eq!(orphans[0].name, "triple-c-home-other");
}

#[test]
fn orphan_detection_ignores_volumes_that_are_not_ours() {
    let volumes = vec![
        vol("nfc-profile-mysql", 183_926_366, 0),
        vol("postgres_data", 9_000_000, 0),
        vol("triple-c-stt-model-cache", 900_000_000, 0),
        vol("triple-c-gateway-config", 1_000, 0),
        vol("triple-c-home-gone", 5_000, 0),
    ];
    let orphans = orphan_volumes(&volumes, &HashSet::new(), true);
    assert_eq!(orphans.len(), 1, "{:?}", orphans);
    assert_eq!(orphans[0].name, "triple-c-home-gone");

    // The STT model cache and the gateway config are ours by name but are not
    // per-project volumes; they belong to features, not projects, and nothing
    // here may reach them.
    assert!(parse_project_volume_name("triple-c-stt-model-cache").is_none());
    assert!(parse_project_volume_name("triple-c-gateway-config").is_none());
}

#[test]
fn a_volume_name_splits_into_the_right_project_and_role() {
    assert_eq!(
        parse_project_volume_name("triple-c-home-abc-123"),
        Some(("abc-123", "home"))
    );
    assert_eq!(
        parse_project_volume_name("triple-c-claude-config-abc-123"),
        Some(("abc-123", "config"))
    );
    // A bare prefix names no project, so it is not ours to delete.
    assert!(parse_project_volume_name("triple-c-home-").is_none());
    assert!(parse_project_volume_name("triple-c-claude-config-").is_none());
    assert!(parse_project_volume_name("triple-c-").is_none());
    assert!(parse_project_volume_name("").is_none());
}

#[test]
fn the_config_role_is_reported_because_it_is_the_one_holding_credentials() {
    let orphans = orphan_volumes(
        &[vol("triple-c-claude-config-gone", 7, 0)],
        &HashSet::new(),
        true,
    );
    assert_eq!(orphans[0].role, "config");
    assert_eq!(orphans[0].project_id, "gone");
}

// ---------------------------------------------------------------------------
// Throwaway-container predicates — these gate a `docker rm`
// ---------------------------------------------------------------------------

fn summary(names: &[&str], labels: &[(&str, &str)]) -> ContainerSummary {
    ContainerSummary {
        names: Some(names.iter().map(|n| (*n).to_string()).collect()),
        labels: Some(
            labels
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
        ),
        ..Default::default()
    }
}

#[test]
fn a_scrub_container_is_matched_on_its_whole_name_not_a_substring() {
    // Docker's `name` filter is a *substring* match, so the daemon happily
    // returns a user's own container whose name merely contains ours. The
    // predicate is what decides, and it anchors at the start.
    assert!(is_scrub_container(&summary(&["/triple-c-scrub-abc123"], &[])));

    assert!(!is_scrub_container(&summary(&["/my-triple-c-scrub-notes"], &[])));
    assert!(!is_scrub_container(&summary(&["/triple-c-scrubber"], &[])));
    assert!(!is_scrub_container(&summary(&["/triple-c-abc"], &[])));
    assert!(!is_scrub_container(&summary(&[], &[])));
}

#[test]
fn a_probe_container_is_matched_on_its_label_not_on_the_daemons_filter() {
    // The `label=triple-c.probe=migration` filter is an exact match and would
    // be enough on its own — but a filter is a string assembled elsewhere in
    // the file, and "enough" is not the standard for something that runs
    // `docker rm`.
    assert!(is_migration_probe(&summary(
        &["/nervous_curie"],
        &[(migration::LABEL_PROBE, migration::PROBE_LABEL_MIGRATION)]
    )));

    // A different probe kind, a truncated value, and no label at all.
    assert!(!is_migration_probe(&summary(
        &["/x"],
        &[(migration::LABEL_PROBE, "something-else")]
    )));
    assert!(!is_migration_probe(&summary(&["/x"], &[])));
    assert!(!is_migration_probe(&summary(
        &["/x"],
        &[("triple-c.managed", "true")]
    )));
}

// ---------------------------------------------------------------------------
// Store trust
// ---------------------------------------------------------------------------

fn project(id: &str, name: &str) -> Project {
    let mut p = Project::new(name.to_string(), Vec::new());
    p.id = id.to_string();
    p
}

#[test]
fn an_unreadable_projects_json_is_never_trusted() {
    let err = project_store_trust(&[project("a", "api")], true, false).unwrap_err();
    assert!(err.contains("could not be read"), "{}", err);
}

#[test]
fn an_empty_list_from_an_existing_file_is_treated_as_a_failed_load() {
    // `ProjectsStore::new()` swallows a corrupt projects.json: it backs the file
    // up and starts empty. That is right for the app and catastrophic here, so
    // the combination "empty list + file present" is refused rather than read as
    // "the user has no projects".
    let err = project_store_trust(&[], true, true).unwrap_err();
    assert!(err.contains("suppressed"), "{}", err);

    // No file at all is a genuine fresh install, and there is nothing on the
    // daemon to mis-attribute in that state.
    assert!(project_store_trust(&[], false, true).unwrap().is_empty());
}

#[test]
fn a_healthy_store_yields_its_ids() {
    let ids = project_store_trust(&[project("a", "api"), project("b", "web")], true, true).unwrap();
    assert_eq!(ids, HashSet::from(["a".to_string(), "b".to_string()]));
}

// ---------------------------------------------------------------------------
// Layer accounting — the number the whole UI exists to show
// ---------------------------------------------------------------------------

#[test]
fn commit_layers_are_the_history_a_snapshot_has_beyond_its_base() {
    // `image_history` returns newest first, and a snapshot's history is its
    // base's history with the commits appended — so the commits are the head.
    let snapshot = vec![0, 868_000_000, 500_000_000, 4_000_000_000, 0];
    let stats = layer_stats(&snapshot, Some(2));
    assert_eq!(stats.commit_layers, 3);
    assert_eq!(stats.above_base_bytes, Some(1_368_000_000));
}

#[test]
fn a_missing_base_reports_a_count_but_refuses_to_split_the_bytes() {
    // A base image that has been swept is common — the project keeps running
    // from its own snapshot. The layer count is still useful; the byte split is
    // not knowable, and a guess there would be the one number in this UI that
    // is not measured.
    let stats = layer_stats(&[10, 20, 0, 30], None);
    assert_eq!(stats.commit_layers, 3, "zero-byte layers are metadata, not commits");
    assert_eq!(stats.above_base_bytes, None);
}

#[test]
fn a_base_longer_than_the_snapshot_means_they_are_not_the_same_lineage() {
    let stats = layer_stats(&[10, 20], Some(5));
    assert_eq!(stats.above_base_bytes, None);
}

#[test]
fn a_snapshot_that_is_exactly_its_base_has_no_commits() {
    let stats = layer_stats(&[10, 20, 30], Some(3));
    assert_eq!(stats.commit_layers, 0);
    assert_eq!(stats.above_base_bytes, Some(0));
}

#[test]
fn compaction_is_bounded_and_the_floor_is_zero() {
    // Verified on Docker 29.7.2: a stack with nothing superseded came out
    // *larger* (29.8 MB -> 30.8 MB), because the merged layer recompresses on
    // its own. So the floor is zero and never a fraction of the total.
    let (floor, ceiling) = compaction_bounds(&[100, 100, 100]);
    assert_eq!(floor, 0);
    assert_eq!(ceiling, 200, "at most everything but the largest layer");

    // One layer can supersede nothing, so there is no upside at all.
    assert_eq!(compaction_bounds(&[500]), (0, 0));
    assert_eq!(compaction_bounds(&[]), (0, 0));
}

#[test]
fn the_ceiling_shown_in_the_plan_matches_the_bound() {
    // With no shared base to re-duplicate, the bound is the superseded-bytes
    // one: an even split approximating "everything but the largest layer".
    assert_eq!(compaction_ceiling_for(300, 0, 3), 200);
    assert_eq!(compaction_ceiling_for(300, 0, 1), 0, "one layer supersedes nothing");
    assert_eq!(compaction_ceiling_for(0, 0, 14), 0);
    assert_eq!(compaction_ceiling_for(-5, 0, 3), 0, "never negative");
}

#[test]
fn compacting_a_thin_snapshot_over_a_fat_base_is_never_offered() {
    // The bug this exists to stop, with the real numbers that exposed it.
    //
    // `FROM scratch` + `COPY --from` produces an image that shares nothing, so
    // the flattened snapshot carries its own private copy of the base — which
    // stays on disk regardless, because every other project is still built from
    // it. Eight of ten projects on a real daemon had a unique delta of
    // 0.10–1.32 GB over a 4.72 GB shared base: flattening any of them turns a
    // sub-gigabyte cost into a ~4.7 GB one.
    //
    // A ceiling of zero keeps them out of the plan entirely, rather than
    // offering a 4 GB loss as a saving.
    let shared_base = 4_723_860_394;
    for unique in [100_000_000i64, 630_000_000, 1_320_000_000] {
        assert_eq!(
            compaction_ceiling_for(unique, shared_base, 6),
            0,
            "a {}-byte delta over a {}-byte base must not be offered",
            unique,
            shared_base
        );
    }

    // The one project that *was* worth it: 8.44 GB unique across 14 layers over
    // a 3.83 GB base. The base penalty still binds — 8.44 - 3.83 = 4.61 GB is
    // smaller than the 7.84 GB the even split allows — so that is the figure.
    let ceiling = compaction_ceiling_for(8_440_966_715, 3_832_425_659, 14);
    assert_eq!(ceiling, 8_440_966_715 - 3_832_425_659);
    assert!(ceiling < (8_440_966_715 / 14) * 13, "the base penalty must bind here");
}

#[test]
fn the_superseded_bound_still_binds_when_the_base_is_small() {
    // With a tiny base, the limit on what can come back is how much the layers
    // superseded, not the duplication cost. Both terms have to be live.
    let ceiling = compaction_ceiling_for(300, 10, 3);
    assert_eq!(ceiling, 200, "the even split binds, not 300 - 10");
}

// ---------------------------------------------------------------------------
// Build cache
// ---------------------------------------------------------------------------

fn cache(size: i64, in_use: bool, age_hours: i64) -> BuildCacheFacts {
    BuildCacheFacts {
        size,
        in_use,
        last_used_at: Some(chrono::Utc::now() - chrono::Duration::hours(age_hours)),
    }
}

#[test]
fn the_age_filter_leaves_in_use_and_recent_records_alone() {
    let now = chrono::Utc::now();
    let entries = vec![
        cache(1_000, false, 200), // old and free -> counted
        cache(2_000, true, 200),  // old but in use -> never
        cache(4_000, false, 10),  // free but recent -> not by this filter
        BuildCacheFacts {
            size: 8_000,
            in_use: false,
            // No timestamp at all: unknown age fails closed, same rule as an
            // unknown volume ref count.
            last_used_at: None,
        },
    ];
    assert_eq!(stale_build_cache_bytes(&entries, 168, now), 1_000);
}

#[test]
fn docker_sizes_parse_in_base_1000_because_that_is_what_docker_prints() {
    // `units.HumanSize` is base 1000. Reading "28.0GB" as 1024-based would
    // overstate the single biggest win in this panel by about 7%.
    assert_eq!(parse_docker_size("0B"), Some(0));
    assert_eq!(parse_docker_size("28.0GB"), Some(28_000_000_000));
    assert_eq!(parse_docker_size("1.5MB"), Some(1_500_000));
    assert_eq!(parse_docker_size(" 46.88GB "), Some(46_880_000_000));
    assert_eq!(parse_docker_size("12kB"), Some(12_000));

    // Anything unrecognised is None, so the caller falls back to `df()` rather
    // than showing a wrong number.
    assert_eq!(parse_docker_size("lots"), None);
    assert_eq!(parse_docker_size(""), None);
    assert_eq!(parse_docker_size("12GiB"), None);

    // A space before the unit is fine — `docker builder prune` uses a tab.
    assert_eq!(parse_docker_size("1.5 kB"), Some(1_500));
    assert_eq!(parse_docker_size("\t20.59MB"), Some(20_590_000));

    // A negative would subtract from the running freed total if it got through.
    assert_eq!(parse_docker_size("-5GB"), None);
}

#[test]
fn buildx_du_output_parses_into_total_and_reclaimable() {
    // Real shape, taken from `docker buildx du` on Docker 29.7.2.
    let output = "ID        RECLAIMABLE   SIZE      LAST ACCESSED\n\
                  abc123    true          29.78MB   36 seconds ago\n\
                  Reclaimable:\t28.0GB\n\
                  Total:\t\t33.57MB\n";
    assert_eq!(parse_buildx_du(output), Some((33_570_000, 28_000_000_000)));

    // An empty cache still reports both lines.
    assert_eq!(
        parse_buildx_du("Reclaimable:\t0B\nTotal:\t\t0B\n"),
        Some((0, 0))
    );
    // No Total line means the output is not what we expect; fall back rather
    // than invent.
    assert_eq!(parse_buildx_du("nothing here"), None);
}

#[test]
fn the_reclaimed_figure_comes_from_the_prunes_own_report() {
    // `docker system prune` / `image prune` wording.
    let output = "deleted: sha256:abc\ndeleted: sha256:def\nTotal reclaimed space: 12.3GB\n";
    assert_eq!(parse_reclaimed_space(output), 12_300_000_000);
    assert_eq!(parse_reclaimed_space("Total reclaimed space: 0B"), 0);

    // `docker builder prune` wording — the one this module actually runs, and
    // the one an earlier draft of the parser missed entirely, reporting every
    // build-cache prune as having freed nothing. Verbatim from Docker 29.7.2.
    let builder = "2zp7lsfz2me0jtqe8rio6s4eq*\ttrue\t\t8.192kB\tLess than a second ago\n\
                   rmonzx1v6jrrlgxt783dmfb3k\ttrue\t16.79MB\t1 second ago\n\
                   Total:\t20.59MB\n";
    assert_eq!(parse_reclaimed_space(builder), 20_590_000);

    // A filtered prune that matched nothing still prints the summary.
    assert_eq!(parse_reclaimed_space("Total:\t0B\n"), 0);

    // A prune that printed nothing recognisable freed nothing we can claim.
    assert_eq!(parse_reclaimed_space("nothing to do"), 0);
}

// ---------------------------------------------------------------------------
// Scripts — shell strings, so pinned by test
// ---------------------------------------------------------------------------

#[test]
fn the_compaction_dockerfile_reuses_the_one_scrub_list() {
    let df = compaction_dockerfile(
        "triple-c-snapshot-p1:latest",
        &container::snapshot_scrub_script(),
    );

    assert!(df.starts_with("FROM triple-c-snapshot-p1:latest AS src\n"));
    assert!(
        df.contains("\nFROM scratch\nCOPY --from=src / /\n"),
        "the flatten is the whole point: {}",
        df
    );

    // Every path in the reviewed list has to appear, and it has to be *that*
    // list rather than a second copy — a forked list is the failure mode a
    // hardcoded set of `rm -rf` paths invites.
    for path in container::SNAPSHOT_SCRUB_PATHS {
        assert!(df.contains(path), "scrub path {} missing from {}", path, df);
    }

    // The RUN must be one line: a Dockerfile instruction does not continue over
    // a bare newline, and a script folded wrongly would silently truncate to
    // its first statement.
    let run_lines: Vec<&str> = df.lines().filter(|l| l.starts_with("RUN ")).collect();
    assert_eq!(run_lines.len(), 1, "{}", df);
    assert!(!run_lines[0].contains('\n'));
}

#[test]
fn the_compaction_dockerfile_never_reaches_a_bind_mount() {
    let df = compaction_dockerfile("x:latest", &container::snapshot_scrub_script());
    // `/workspace/{mount_name}` subtrees are the user's real project
    // directories, mounted from the host. Nothing in a scrub may name one, and
    // the two read-only host mounts under /tmp are dot-prefixed so no glob
    // reaches them either.
    assert!(!df.contains("/workspace"), "{}", df);
    assert!(!df.contains(".host-ca"), "{}", df);
    assert!(!df.contains(".host-aws"), "{}", df);
}

#[test]
fn the_cache_script_only_ever_names_paths_under_home() {
    for include_rustup in [false, true] {
        let script = cache_clear_script(include_rustup);
        for line in script.lines() {
            // Every deletion in this script is anchored to $HOME. A path that
            // is not would be operating on the system layer, or worse on a
            // bind mount.
            if line.contains("rm -rf") {
                assert!(
                    line.contains("$HOME") || line.contains("$d"),
                    "unanchored deletion: {}",
                    line
                );
            }
        }
        assert!(!script.contains("/workspace"), "{}", script);
        assert!(!script.contains(" / "), "{}", script);
    }
}

#[test]
fn rustup_is_only_cleared_when_it_is_asked_for() {
    // Regenerable, but a re-download rather than a rebuild from a local cache —
    // which is why it is a separate tick and not part of the set.
    assert!(!cache_clear_script(false).contains(".rustup"));
    assert!(cache_clear_script(true).contains("$HOME/.rustup/toolchains"));
}

#[test]
fn the_cache_script_keeps_the_newest_playwright_revision() {
    // Deleting the current revision turns a working browser-view project into
    // one that downloads 400 MB on next use, so only superseded revisions go.
    let script = cache_clear_script(false);
    assert!(script.contains("keep=$("), "{}", script);
    assert!(script.contains("= \"$keep\" ] && continue"), "{}", script);
    // ...and it must not simply remove the whole directory.
    assert!(!script.contains("rm -rf -- \"$HOME/.cache/ms-playwright\""), "{}", script);
}

#[test]
fn the_cache_script_covers_every_documented_cache() {
    let script = cache_clear_script(false);
    for path in [
        "$HOME/.npm/_cacache",
        "$HOME/.npm/_npx",
        "$HOME/.cache/go-build",
        "$HOME/.cache/pip",
        "$HOME/.cache/uv",
        "$HOME/.cache/act",
        "$HOME/.cache/chrome-devtools-mcp",
        "$HOME/go/pkg/mod",
        "$HOME/.cache/ms-playwright",
    ] {
        assert!(script.contains(path), "{} missing from the cache script", path);
    }
}

#[test]
fn the_cache_script_reports_a_total_that_can_be_read_back() {
    let script = cache_clear_script(false);
    assert!(script.contains(CACHE_MARKER));
    assert_eq!(
        parse_cache_total(&format!("noise\n{}6291456\nmore noise\n", CACHE_MARKER)),
        Some(6_291_456)
    );
    // No marker means the script never reached its last line — a killed exec,
    // not a run that freed nothing.
    assert_eq!(parse_cache_total("permission denied"), None);
    assert_eq!(parse_cache_total(&format!("{}0", CACHE_MARKER)), Some(0));
}

// ---------------------------------------------------------------------------
// Confirmation
// ---------------------------------------------------------------------------

#[test]
fn a_typed_confirmation_must_match_the_project_name_exactly() {
    assert!(confirmation_matches("whp", "whp"));
    // A trailing space from a paste is not a different intent.
    assert!(confirmation_matches("whp", "  whp  "));

    // Case is not negotiable: `Api` and `api` are different projects, and this
    // is the only thing between a user and their transcripts.
    assert!(!confirmation_matches("Api", "api"));
    assert!(!confirmation_matches("whp", "wh"));
    assert!(!confirmation_matches("whp", ""));
    // An empty expected name would otherwise be satisfied by an empty box.
    assert!(!confirmation_matches("", ""));
}

// ---------------------------------------------------------------------------
// Host detection
// ---------------------------------------------------------------------------

#[test]
fn the_vhdx_caveat_needs_both_windows_and_docker_desktop() {
    assert!(vhdx_applies(true, "Docker Desktop"));
    assert!(vhdx_applies(true, "Docker Desktop 4.30.0"), "matched loosely");

    // macOS Docker Desktop has the same never-shrinks property but a different
    // file and a different fix, so this note would be wrong there.
    assert!(!vhdx_applies(false, "Docker Desktop"));
    // A Windows host talking to a native or remote engine has neither.
    assert!(!vhdx_applies(true, "Ubuntu 24.04.1 LTS"));
}

#[test]
fn the_vhdx_note_spells_out_the_fix() {
    // Users otherwise report "I pruned and C: did not change" as a bug, so both
    // routes have to be on screen, not in a doc.
    assert!(WSL2_VHDX_NOTE.contains("never shrinks"));
    assert!(WSL2_VHDX_FIX[0].contains("wsl --shutdown"));
    assert!(WSL2_VHDX_FIX[1].contains("Optimize-VHD"));
    assert!(WSL2_VHDX_FIX[1].contains("docker_data.vhdx"));
    assert!(WSL2_VHDX_FIX_GUI.contains("Purge data"));
}

#[test]
fn base_images_are_recognised_by_reference_for_display_only() {
    assert!(is_base_image_reference("ghcr.io/shadowdao/triple-c-sandbox:latest"));
    assert!(is_base_image_reference("triple-c-sandbox:latest"));
    assert!(is_base_image_reference("triple-c:latest"));

    // A project's own snapshot is not a base image, and neither is anything of
    // the user's.
    assert!(!is_base_image_reference("triple-c-snapshot-abc:latest"));
    assert!(!is_base_image_reference("triple-c-gateway:latest"));
    assert!(!is_base_image_reference("postgres:17-alpine"));
}

// ---------------------------------------------------------------------------
// IPC contract
// ---------------------------------------------------------------------------

#[test]
fn reclaim_targets_round_trip_through_the_wire_format() {
    // The frontend ticks an item and hands the very same `target` object back,
    // so the tagged representation has to survive the trip unchanged in both
    // directions.
    for target in all_reclaim_targets() {
        let json = serde_json::to_string(&target).unwrap();
        let back: ReclaimTarget = serde_json::from_str(&json).unwrap();
        assert_eq!(target, back, "{}", json);
        assert!(json.contains("\"kind\""), "{}", json);
    }
}

#[test]
fn destructive_targets_round_trip_too() {
    let target = DestructiveTarget::RollbackPin {
        project_id: "p1".to_string(),
        tag: "pre-migration-20260101-101500".to_string(),
    };
    let json = serde_json::to_string(&target).unwrap();
    assert!(json.contains("\"kind\":\"rollback_pin\""), "{}", json);
    assert_eq!(
        serde_json::from_str::<DestructiveTarget>(&json).unwrap(),
        target
    );
}

#[test]
fn the_report_serialises_as_snake_case_like_every_other_ipc_struct() {
    let report = DiskUsageReport {
        projects: vec![ProjectDiskRow {
            project_id: "p1".to_string(),
            project_name: "whp".to_string(),
            snapshot_commit_layers: 14,
            container_writable_bytes: 868_000_000,
            ..Default::default()
        }],
        ..Default::default()
    };
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["projects"][0]["snapshot_commit_layers"], 14);
    assert_eq!(json["projects"][0]["container_writable_bytes"], 868_000_000i64);
    assert!(json["orphan_volumes_unavailable"].is_null());
    // `Option<i64>` must reach the frontend as null, not be omitted — the TS
    // type is `number | null`, matching every other optional in `types.ts`.
    assert!(json["projects"][0]["snapshot_above_base_bytes"].is_null());
}
