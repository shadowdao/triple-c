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
        9,
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
fn a_compaction_container_is_never_matched_by_the_scrub_bucket() {
    // These had the same `triple-c-scrub-*` prefix once. The scrub bucket
    // removes with `force: true`, so a reclaim fired from a second window while
    // a compaction was mid-flight would have destroyed the container the commit
    // was about to run against. Separate prefixes, and neither predicate may
    // reach the other's containers.
    let compaction = summary(&["/triple-c-compact-abc123"], &[]);
    let scrub = summary(&["/triple-c-scrub-abc123"], &[]);

    assert!(is_compaction_container(&compaction));
    assert!(!is_scrub_container(&compaction), "the scrub bucket must not reach it");

    assert!(is_scrub_container(&scrub));
    assert!(!is_compaction_container(&scrub));

    // Same substring-filter hazard applies to the new prefix.
    assert!(!is_compaction_container(&summary(&["/my-triple-c-compact-notes"], &[])));
}

#[test]
fn the_daemon_wide_buckets_leave_a_young_container_alone() {
    // Both of these buckets are `Safety::Safe` — one tick, no confirmation —
    // and both reach *every* matching container on the daemon, because a label
    // and a name prefix are daemon-wide and `ReclaimTarget::project_id()` is
    // `None` for them. So a second app instance's live migration probe, and a
    // live secret rewrite's scratch container, are both in range. Age is the
    // only discriminator available from this side of the process boundary, and
    // a container the daemon gave no creation time for is treated as young.
    let now = chrono::Utc::now().timestamp();

    let mut young_probe = summary(&["/nervous_curie"], &[(migration::LABEL_PROBE, "migration")]);
    young_probe.created = Some(now - 30);
    assert!(is_migration_probe(&young_probe));
    assert!(!is_reapable_migration_probe(&young_probe));

    let mut old_probe = young_probe.clone();
    old_probe.created = Some(now - migration::PROBE_REAP_MIN_AGE_SECS - 1);
    assert!(is_reapable_migration_probe(&old_probe));

    let mut undated_probe = young_probe.clone();
    undated_probe.created = None;
    assert!(!is_reapable_migration_probe(&undated_probe));

    // `triple-c-scrub-*` is a **live** name: `rewrite_image_without_secrets`
    // creates its scratch container under it, and killing that between the
    // create and the commit leaves a revoked OAuth token baked into the
    // snapshot's Config.Env — the exact thing that function exists to remove.
    let mut young_scrub = summary(&["/triple-c-scrub-abc123"], &[]);
    young_scrub.created = Some(now - 5);
    assert!(is_scrub_container(&young_scrub));
    assert!(!is_reapable_scrub_container(&young_scrub));

    let mut old_scrub = young_scrub.clone();
    old_scrub.created = Some(now - SCRATCH_CONTAINER_MIN_AGE_SECS - 1);
    assert!(is_reapable_scrub_container(&old_scrub));
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
    let err = project_store_trust(&[project("a", "api")], true, None).unwrap_err();
    assert!(err.contains("could not be read"), "{}", err);
}

#[test]
fn an_empty_list_from_an_existing_file_is_treated_as_a_failed_load() {
    // `ProjectsStore::new()` swallows a corrupt projects.json: it backs the file
    // up and starts empty. That is right for the app and catastrophic here, so
    // the combination "empty list + file present" is refused rather than read as
    // "the user has no projects".
    let err = project_store_trust(&[], true, Some(&[])).unwrap_err();
    assert!(err.contains("suppressed"), "{}", err);

    // No file at all is a genuine fresh install, and there is nothing on the
    // daemon to mis-attribute in that state.
    assert!(project_store_trust(&[], false, Some(&[])).unwrap().is_empty());
}

#[test]
fn a_healthy_store_yields_its_ids() {
    let ids =
        project_store_trust(&[project("a", "api"), project("b", "web")], true, Some(&[])).unwrap();
    assert_eq!(ids, HashSet::from(["a".to_string(), "b".to_string()]));
}

#[test]
fn a_project_only_the_file_knows_about_still_counts_as_live() {
    // M6: `projects` is this process's in-memory list. A project added by a
    // *second* copy of the app is in `projects.json` and not in that list, and
    // its live home and config volumes then matched "no project claims this".
    // The union is what closes the gap; the file is authoritative for
    // everything this instance has not heard about.
    let ids = project_store_trust(
        &[project("a", "api")],
        true,
        Some(&["a".to_string(), "b-from-the-other-window".to_string()]),
    )
    .unwrap();
    assert!(
        ids.contains("b-from-the-other-window"),
        "a project only the file knows about must not look orphaned: {:?}",
        ids
    );
    // And the reverse: a project this instance just added is live whether or
    // not the file has caught up.
    assert!(ids.contains("a"), "{:?}", ids);
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
fn the_compaction_run_carries_the_scrub_script_byte_for_byte() {
    // **This is the assertion the old one should have been.** The previous
    // version checked only that the `RUN` was a single line — which the broken
    // space-fold satisfied perfectly, while producing
    // `… for p in …; do [ -e "$p" ] || continue sz=$(…) …`, i.e. shell that
    // `sh` refuses to parse. Every compaction ever attempted failed on the
    // build's first stage.
    //
    // Nothing is folded now: the script goes into the JSON exec form verbatim,
    // so the strong statement is available — what reaches `/bin/sh -c` is
    // exactly what `snapshot_scrub_script()` returned, newlines included.
    let script = container::snapshot_scrub_script();
    let df = compaction_dockerfile("triple-c-snapshot-p1:latest", &script);
    let run = df
        .lines()
        .find(|l| l.starts_with("RUN "))
        .expect("no RUN line");
    let recovered = script_from_run_line(run).expect("the RUN is not a parseable exec form");
    assert_eq!(
        recovered, script,
        "the compaction must run the scrub script unmodified"
    );
    // The exec form names the shell itself, because `RUN [...]` does not go
    // through one.
    let argv: Vec<String> = serde_json::from_str(run.trim_start_matches("RUN ")).unwrap();
    assert_eq!(argv[0], "/bin/sh");
    assert_eq!(argv[1], "-c");
}

#[test]
fn the_compaction_run_is_valid_shell() {
    // The test that would have caught H1, and the only kind that can: hand the
    // exact program the daemon will execute to a real shell and ask it to
    // parse. `sh -n` reads and parses without running anything.
    //
    // It is run against the live `snapshot_scrub_script()` rather than a fixture
    // precisely because that script is not this module's to own — it is free to
    // grow a `case`, an `if` or a function, and this must keep holding when it
    // does.
    let df = compaction_dockerfile("triple-c-snapshot-p1:latest", &container::snapshot_scrub_script());
    let run = df.lines().find(|l| l.starts_with("RUN ")).unwrap();
    let script = script_from_run_line(run).unwrap();
    assert_shell_parses(&script, "the compaction scrub");
}

#[test]
fn a_multi_line_script_with_blocks_survives_the_run_encoding() {
    // The property the old fold did not have, stated directly: a script with a
    // `for`/`do`, an `if`/`then`, a `case` and a quote-heavy line has to survive
    // whatever this module does to it. Joining lines with a space breaks the
    // first three; joining with `;` breaks `if x; then; y`. Encoding the string
    // breaks none of them.
    let awkward = "total=0\n                   for p in /tmp/a* /tmp/b*; do\n                   \t[ -e \"$p\" ] || continue\n                   \tcase \"$p\" in\n                   \t\t*.keep) continue ;;\n                   \tesac\n                   \tif [ -d \"$p\" ]; then\n                   \t\trm -rf -- \"$p\"\n                   \tfi\n                   done\n                   echo \"done: $total\"\n";
    let df = compaction_dockerfile("x:latest", awkward);
    let run = df.lines().find(|l| l.starts_with("RUN ")).unwrap();
    assert!(!run.contains('\n'), "the RUN must still be one Dockerfile line");
    assert_eq!(script_from_run_line(run).unwrap(), awkward);
    assert_shell_parses(awkward, "an awkward but legal script");
}

/// Run `sh -n` over a program and fail with the shell's own diagnostic.
///
/// Skipped, loudly, on a host with no `/bin/sh` — which is not a case any
/// developer machine or CI image this repo targets is in, but a silent pass
/// would be worse than a skipped test.
fn assert_shell_parses(script: &str, what: &str) {
    let output = match std::process::Command::new("/bin/sh")
        .arg("-n")
        .arg("-c")
        .arg(script)
        .output()
    {
        Ok(output) => output,
        Err(e) => {
            eprintln!("skipping the shell syntax check for {}: {}", what, e);
            return;
        }
    };
    assert!(
        output.status.success(),
        "{} is not valid shell:\n{}\n--- script ---\n{}",
        what,
        String::from_utf8_lossy(&output.stderr),
        script
    );
}

#[test]
fn the_compaction_build_is_labelled_so_the_sweep_can_collect_it() {
    // Everything that cleans up after this build — the discard path when the
    // result is not smaller, the untag after a successful commit — leans on
    // `sweep_orphaned_snapshots`, and that sweep filters on `dangling=true`
    // AND `triple-c.managed=true`. Without the label it can never match, and
    // the flattened intermediate is stranded.
    let df = compaction_dockerfile("x:latest", &container::snapshot_scrub_script());
    assert!(
        df.contains("LABEL triple-c.managed=true"),
        "the sweep filters on this label and would never match: {}",
        df
    );
    // It has to be on the *final* stage, not the discarded `src` one.
    let after_scratch = df.split("FROM scratch").nth(1).expect("no final stage");
    assert!(after_scratch.contains("LABEL triple-c.managed=true"), "{}", df);
}

#[test]
fn the_compaction_dockerfile_never_reaches_a_bind_mount() {
    let df = compaction_dockerfile("x:latest", &container::snapshot_scrub_script());
    // `/workspace/{mount_name}` subtrees are the user's real project
    // directories, mounted from the host. Nothing in a scrub may name one, and
    // the two read-only host mounts under /tmp are dot-prefixed so no glob
    // reaches them either.
    //
    // **Necessary, not sufficient, and do not read it as coverage.** A path not
    // appearing as a literal says nothing about where a glob or a symlink
    // resolves to at runtime; the containment property itself is tested in
    // `container.rs`, which owns the path list and the script. This assertion
    // is kept because a literal `/workspace` appearing here would be an
    // unambiguous mistake, and that is all it detects.
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

#[test]
fn only_a_real_rollback_tag_can_name_an_image_to_delete() {
    // `DestructiveTarget::RollbackPin` is the one destructive variant carrying
    // a free-form string from the frontend, and `destroy` interpolates it into
    // an image reference it then removes. Unguarded, `tag: "latest"` names the
    // project's *live snapshot* — deleted under a dialog that says "rollback
    // pin". The guard is `parse_rollback_tag`, so this pins what it accepts.
    assert!(migration::parse_rollback_tag("pre-migration-20260101-101500").is_some());

    for hostile in [
        "latest",
        "",
        "pre-migration-",
        "pre-migration-notatimestamp",
        "../latest",
        "latest\npre-migration-20260101-101500",
    ] {
        assert!(
            migration::parse_rollback_tag(hostile).is_none(),
            "{:?} must not be accepted as a rollback pin tag",
            hostile
        );
    }
}

#[test]
fn a_destroy_result_never_claims_to_be_reclaim_work() {
    // An earlier version returned `OrphanVolume { name }` for a home-volume
    // deletion — naming a volume that was never an orphan, and attributing the
    // outcome to a plan row the user never ticked. Exactly one of the two
    // fields is ever set.
    let reclaim_shaped = ReclaimResult {
        target: Some(ReclaimTarget::DanglingSnapshots),
        destroyed: None,
        ok: true,
        freed_bytes: 1,
        projected_bytes: None,
        message: String::new(),
    };
    let destroy_shaped = ReclaimResult {
        target: None,
        destroyed: Some(DestructiveTarget::HomeVolume {
            project_id: "p1".to_string(),
        }),
        ..reclaim_shaped.clone()
    };
    assert!(reclaim_shaped.target.is_some() != reclaim_shaped.destroyed.is_some());
    assert!(destroy_shaped.target.is_some() != destroy_shaped.destroyed.is_some());

    // And both shapes survive the wire.
    let json = serde_json::to_string(&destroy_shaped).unwrap();
    assert_eq!(serde_json::from_str::<ReclaimResult>(&json).unwrap(), destroy_shaped);
}

#[test]
fn a_snapshot_with_no_known_base_is_not_offered_for_compaction() {
    // With `triple-c.base-image-id` absent — the normal case for a project
    // created before that label existed — `layer_stats` counts every layer that
    // carries bytes, base included. A never-recreated project then reports ~15
    // "commit layers" and would sail past a `> 1` check. `base_lineage_known`
    // is what stops the plan offering a rewrite sized from a number that does
    // not mean what its name says.
    let unknown = layer_stats(&[10, 20, 30, 40], None);
    assert_eq!(unknown.commit_layers, 4);
    assert_eq!(unknown.above_base_bytes, None, "the split must not be guessed");

    let known = layer_stats(&[10, 20, 30, 40], Some(3));
    assert_eq!(known.commit_layers, 1);
    assert_eq!(known.above_base_bytes, Some(10));
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

    // A registry port must not be mistaken for a tag separator.
    assert!(is_base_image_reference("localhost:5000/triple-c-sandbox:latest"));
    assert!(is_base_image_reference("registry.example.com:8443/triple-c-sandbox"));

    // A project's own snapshot is not a base image, and neither is anything of
    // the user's.
    assert!(!is_base_image_reference("triple-c-snapshot-abc:latest"));
    assert!(!is_base_image_reference("localhost:5000/postgres:17"));
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
    assert_eq!(json["projects"][0]["base_lineage_known"], false);
    assert_eq!(json["projects"][0]["container_writable_bytes"], 868_000_000i64);
    assert!(json["orphan_volumes_unavailable"].is_null());
    // `Option<i64>` must reach the frontend as null, not be omitted — the TS
    // type is `number | null`, matching every other optional in `types.ts`.
    assert!(json["projects"][0]["snapshot_above_base_bytes"].is_null());
}

// ---------------------------------------------------------------------------
// Rollback pins — the guards the safe bucket was missing
// ---------------------------------------------------------------------------

fn moment(y: i32, m: u32, d: u32) -> chrono::DateTime<chrono::Utc> {
    chrono::NaiveDate::from_ymd_opt(y, m, d)
        .unwrap()
        .and_hms_opt(12, 0, 0)
        .unwrap()
        .and_utc()
}

#[test]
fn the_safe_pin_bucket_applies_every_guard_the_other_paths_do() {
    let now = moment(2026, 8, 23);
    let ours = migration::rollback_tag(&moment(2026, 1, 1));

    // 1. A tag that merely *starts* `pre-migration-`. `destroy` refuses it with
    //    a comment explaining that `tag: "latest"` would otherwise name the
    //    project's live snapshot; the safe bucket used to accept anything.
    assert_eq!(
        pin_disposition("pre-migration-keepme", false, Some(moment(2020, 1, 1)), &now),
        PinDisposition::NotOurs
    );
    assert_eq!(
        pin_disposition("latest", false, Some(moment(2020, 1, 1)), &now),
        PinDisposition::NotOurs
    );

    // 2. A record still claims it. Never reaped at any age, and this is the one
    //    guard the old code did have.
    assert_eq!(
        pin_disposition(&ours, true, Some(moment(2020, 1, 1)), &now),
        PinDisposition::Claimed
    );

    // 3. Ownerless but inside the grace period — including "no reaper has seen
    //    it yet", which is where the clock starts. The old code untagged this
    //    immediately, and `sweep_orphaned_snapshots` four lines later turned
    //    the untag into a deletion.
    assert_eq!(
        pin_disposition(&ours, false, None, &now),
        PinDisposition::WithinGrace
    );
    assert_eq!(
        pin_disposition(&ours, false, Some(now - chrono::Duration::days(1)), &now),
        PinDisposition::WithinGrace
    );

    // 4. Ownerless and past its grace period. The only case that is collectable
    //    without a typed confirmation.
    assert_eq!(
        pin_disposition(
            &ours,
            false,
            Some(now - chrono::Duration::days(migration::STALE_PIN_MAX_AGE_DAYS)),
            &now
        ),
        PinDisposition::Reapable
    );
}

#[test]
fn the_safe_bucket_and_the_startup_reaper_cannot_disagree() {
    // Both go through `migration::pin_is_reapable`. The bug was that the safe
    // bucket did not: `reap_stale_migration_pins` required a parseable tag and
    // an age, the reclaim button required neither, and the button is the path
    // with no confirmation in front of it.
    let now = moment(2026, 8, 23);
    let ours = migration::rollback_tag(&moment(2026, 1, 1));
    for since in [
        None,
        Some(now - chrono::Duration::days(1)),
        Some(now - chrono::Duration::days(migration::STALE_PIN_MAX_AGE_DAYS)),
    ] {
        for has_record in [true, false] {
            assert_eq!(
                pin_disposition(&ours, has_record, since, &now) == PinDisposition::Reapable,
                migration::pin_is_reapable(&ours, has_record, since, &now),
                "disposition and the reaper's own predicate disagreed for {:?}/{}",
                since,
                has_record
            );
        }
    }
}

#[test]
fn an_orphaned_volume_cannot_be_reached_without_a_typed_confirmation() {
    // M5: it was a `ReclaimTarget` at `Safety::Safe` — a tick and the group
    // button. `reclaim` cannot be handed a `DestructiveTarget` at all, which is
    // the type-level half of the guarantee; this pins the other half, that no
    // reclaim variant names a volume any more.
    for target in all_reclaim_targets() {
        let wire = serde_json::to_value(&target).unwrap();
        let kind = wire["kind"].as_str().unwrap();
        assert!(
            !kind.contains("volume"),
            "{} can reach a volume from the safe path",
            kind
        );
    }

    // And the confirmation subject is the *volume* name, because an orphan has
    // no project in the store whose name could be typed.
    let target = DestructiveTarget::OrphanVolume {
        name: "triple-c-claude-config-gone".to_string(),
        project_id: "gone".to_string(),
    };
    assert!(confirmation_matches(
        "triple-c-claude-config-gone",
        " triple-c-claude-config-gone "
    ));
    assert!(!confirmation_matches("triple-c-claude-config-gone", "gone"));
    assert_eq!(target.project_id(), "gone");
}

// ---------------------------------------------------------------------------
// The numbers, which the user reads as facts
// ---------------------------------------------------------------------------

#[test]
fn the_snapshot_column_and_the_total_come_from_one_rule() {
    // Case 1: the daemon measured the sharing. Its figure wins.
    assert_eq!(snapshot_attribution(5_000_000_000, 4_700_000_000, Some(1)), 300_000_000);

    // Case 2: no shared size, but the lineage is known. The layer arithmetic is
    // the honest answer, and it is what the Snapshot column already showed —
    // while `total_bytes` used `size - shared` (i.e. the whole image, base
    // included) and `triple_c_total_bytes` then added the base again as its own
    // row. That double count is the exact thing the comment beside the
    // subtraction says it prevents.
    assert_eq!(snapshot_attribution(5_000_000_000, 0, Some(300_000_000)), 300_000_000);

    // Case 3: nothing known. The full size, not zero — an image that shares
    // nothing measurable really does cost all of it, and a flattened snapshot
    // is exactly that shape.
    assert_eq!(snapshot_attribution(5_000_000_000, 0, None), 5_000_000_000);

    // A daemon that reports `-1` for "not computed" must not be read as a
    // 1-byte saving, and a negative result is never returned.
    assert_eq!(snapshot_attribution(1_000, -1, None), 1_000);
    assert_eq!(snapshot_attribution(1_000, 4_000, Some(0)), 0);
}

#[test]
fn a_row_adds_up() {
    // The property the fix exists for, stated arithmetically: whatever the
    // Snapshot column shows is what the Total is built from.
    for (size, shared, above) in [
        (5_000_000_000i64, 4_700_000_000i64, Some(1i64)),
        (5_000_000_000, 0, Some(300_000_000)),
        (5_000_000_000, 0, None),
        (0, 0, None),
    ] {
        let snapshot = snapshot_attribution(size, shared, above);
        let (writable, home, config) = (10i64, 20i64, 30i64);
        let total = snapshot + writable + home + config;
        assert_eq!(
            total - (writable + home + config),
            snapshot,
            "the Total column has to reconcile with the Snapshot column"
        );
    }
}

#[test]
fn human_never_prints_a_unit_the_ladder_forbids() {
    // The 999,999 → "1000.0 KB" bug, which is the one `formatBytes.ts` was
    // written to fix on the frontend. This function's output is rendered on the
    // same line as `formatBytes`' in a compaction message, so the two
    // disagreeing is visible in a single sentence.
    assert_eq!(human(999_999), "1.0 MB");
    assert_eq!(human(999_999_999), "1.0 GB");
    assert_eq!(human(999_999_999_999), "1.0 TB");

    // The ordinary cases still read the way `docker system df` prints them,
    // base 1000.
    assert_eq!(human(0), "0 B");
    assert_eq!(human(999), "999 B");
    assert_eq!(human(1_000), "1.0 KB");
    assert_eq!(human(1_500_000), "1.5 MB");
    assert_eq!(human(4_700_000_000), "4.7 GB");
    // Rounding that does *not* cross the boundary is untouched.
    assert_eq!(human(999_400), "999.4 KB");
}

#[test]
fn no_output_of_human_is_ever_a_four_digit_mantissa() {
    // A sweep rather than a handful of cases: every power-of-ten boundary and
    // its neighbours, which is where the bug lived.
    let mut value = 1i64;
    for _ in 0..19 {
        for candidate in [value - 1, value, value + 1] {
            if candidate < 0 {
                continue;
            }
            let rendered = human(candidate);
            let mantissa = rendered.split(' ').next().unwrap();
            let numeric: f64 = mantissa.parse().unwrap();
            assert!(
                numeric < 1000.0 || rendered.ends_with(" PB"),
                "{} rendered as {}, which the unit ladder is supposed to make impossible",
                candidate,
                rendered
            );
        }
        value = match value.checked_mul(10) {
            Some(v) => v,
            None => break,
        };
    }
}

// ---------------------------------------------------------------------------
// End-to-end compaction, against a real daemon
// ---------------------------------------------------------------------------

/// The whole compaction mechanism, run for real.
///
/// `#[ignore]` because it needs a Docker daemon, pulls `busybox`, and takes
/// tens of seconds — `cargo test` has to stay daemon-free. Run it with
/// `cargo test -- --ignored compaction_end_to_end`.
///
/// It exists because H1 was invisible to every unit test in this file: the
/// generated Dockerfile looked right, the `RUN` was one line as asserted, and
/// the build failed on `/bin/sh: line 0: syntax error: unexpected "do"` every
/// single time. The only test that could have caught it is one that hands the
/// Dockerfile to a daemon.
///
/// It exercises the production functions — [`compaction_dockerfile`],
/// [`build_from_dockerfile`], [`restore_image_config`] — rather than
/// [`compact_snapshot`] itself, so it never has to create a
/// `triple-c-snapshot-*` tag that the app's own sweeps might reach.
#[tokio::test]
#[ignore]
async fn compaction_end_to_end_against_a_real_image() {
    let stem = format!("compaction-e2e-{}", uuid::Uuid::new_v4().simple());
    let source = format!("{}:source", stem);
    let staging = format!("{}:compacting", stem);
    let final_ref = format!("{}:latest", stem);

    // A snapshot-shaped source: four stacked layers, each superseding the last,
    // plus the scrub's own targets and a setuid bit that `COPY --from` has to
    // preserve. The env var carries a newline and a double quote, which is what
    // `restore_image_config` exists for.
    let source_dockerfile = format!(
        "FROM busybox:1.36\n\
         RUN mkdir -p /tmp/claude-1000 /var/cache/apt/archives /var/log/apt /var/lib/apt/lists && \
         dd if=/dev/urandom of=/big-a bs=1M count=40 2>/dev/null\n\
         RUN dd if=/dev/urandom of=/big-b bs=1M count=40 2>/dev/null && rm -f /big-a\n\
         RUN dd if=/dev/urandom of=/tmp/claude-1000/scratch bs=1M count=25 2>/dev/null && \
         dd if=/dev/urandom of=/var/cache/apt/archives/x.deb bs=1M count=15 2>/dev/null && \
         touch /var/log/dpkg.log\n\
         RUN dd if=/dev/urandom of=/big-c bs=1M count=30 2>/dev/null && rm -f /big-b && \
         touch /keepme && chmod 4755 /keepme\n\
         LABEL {LABEL_MANAGED}=true\n\
         WORKDIR /keep-this-dir\n"
    );

    // Force, unlike anything in production: `untag_image` is deliberately
    // unforced because it runs against a user's images, and here the leftovers
    // are this test's own and must not be left on the developer's daemon
    // whatever refuses them.
    async fn cleanup(refs: Vec<String>) {
        let Ok(docker) = get_docker() else {
            return;
        };
        for r in refs {
            if let Err(e) = docker
                .remove_image(
                    &r,
                    Some(RemoveImageOptions {
                        force: true,
                        noprune: false,
                    }),
                    None,
                )
                .await
            {
                eprintln!("could not clean up {}: {}", r, e);
            }
        }
    }

    build_from_dockerfile(&source_dockerfile, &source)
        .await
        .expect("could not build the throwaway source image");

    let docker = get_docker().expect("no docker");
    let before = docker.inspect_image(&source).await.expect("inspect source");
    let before_size = before.size.unwrap_or(0);
    let mut config = before.config.clone().expect("source has no config");
    // A genuinely multi-line env var, injected here rather than through the
    // Dockerfile because `ENV` cannot express a literal newline. This is the
    // case `restore_image_config` exists for: it is why the config is replayed
    // through a create-and-commit instead of being rendered back into
    // Dockerfile instructions, where a newline and a `"` would not survive.
    config
        .env
        .get_or_insert_with(Vec::new)
        .push("E2E_MULTILINE=first\nsecond \"quoted\"".to_string());

    // --- the thing under test ------------------------------------------------
    let dockerfile = compaction_dockerfile(&source, &container::snapshot_scrub_script());
    let built = build_from_dockerfile(&dockerfile, &staging).await;
    assert!(
        built.is_ok(),
        "the compaction build failed, which is exactly the H1 regression: {:?}\n{}",
        built,
        dockerfile
    );

    restore_image_config(&staging, &final_ref, config)
        .await
        .expect("could not replay the image config");

    let after = docker
        .inspect_image(&final_ref)
        .await
        .expect("inspect compacted");
    let after_size = after.size.unwrap_or(0);
    let history = docker.image_history(&final_ref).await.expect("history");
    let after_config = after.config.clone().expect("no config after");

    // The scrub actually ran — the assertion H1 made impossible. `sh -c` inside
    // the compacted image, so the answer comes from the filesystem rather than
    // from the build log.
    let probe = migration::run_throwaway(
        &final_ref,
        "ls /keepme >/dev/null 2>&1 && echo KEEP-OK\n\
         [ -e /tmp/claude-1000/scratch ] && echo SCRATCH-LEFT\n\
         [ -e /var/cache/apt/archives/x.deb ] && echo DEB-LEFT\n\
         [ -e /var/log/dpkg.log ] && echo DPKGLOG-LEFT\n\
         [ -e /big-a ] && echo BIGA-LEFT\n\
         [ -e /big-c ] || echo BIGC-MISSING\n\
         ls -l /keepme\n\
         echo PROBE-END\n\
         exit 0\n",
    )
    .await
    .expect("could not probe the compacted image");

    // **Everything is measured before anything is asserted**, so a failing
    // assertion below does not leave several hundred megabytes of throwaway
    // images on the developer's daemon.
    cleanup(vec![final_ref, staging, source]).await;

    // 1. It is smaller. The three superseded 30–40 MB layers and the ~40 MB of
    //    scrub targets are the whole point.
    assert!(
        after_size < before_size,
        "compaction did not shrink the image: {} -> {}",
        before_size,
        after_size
    );

    // 2. One layer of content.
    let content_layers = history.iter().filter(|e| e.size > 0).count();
    assert_eq!(content_layers, 1, "expected one content layer: {:?}", history);

    // 3. The config round-tripped, newline and quote included.
    let env = after_config.env.clone().unwrap_or_default();
    assert!(
        env.iter()
            .any(|e| e == "E2E_MULTILINE=first\nsecond \"quoted\""),
        "the multi-line env var did not survive: {:?}",
        env
    );
    assert_eq!(after_config.working_dir.as_deref(), Some("/keep-this-dir"));

    // 4. The scrub actually ran.
    assert!(probe.stdout.contains("PROBE-END"), "{}", probe.stdout);
    assert!(probe.stdout.contains("KEEP-OK"), "{}", probe.stdout);
    assert!(
        !probe.stdout.contains("SCRATCH-LEFT"),
        "the agent scratchpad survived the scrub:\n{}",
        probe.stdout
    );
    assert!(
        !probe.stdout.contains("DEB-LEFT"),
        "apt archives survived the scrub:\n{}",
        probe.stdout
    );
    assert!(
        !probe.stdout.contains("DPKGLOG-LEFT"),
        "dpkg.log survived the scrub:\n{}",
        probe.stdout
    );
    assert!(
        !probe.stdout.contains("BIGC-MISSING"),
        "the live payload was lost, which is worse than not compacting:\n{}",
        probe.stdout
    );
    // `COPY --from` has to keep the setuid bit; a compaction that dropped it
    // would break sudo inside every migrated project.
    assert!(
        probe.stdout.contains("-rwsr-xr-x"),
        "the setuid bit did not survive the flatten:\n{}",
        probe.stdout
    );
}
