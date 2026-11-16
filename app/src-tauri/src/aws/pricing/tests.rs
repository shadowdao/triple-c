use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::*;
use crate::models::project::{Arch, AwsWorkspaceConfig, StorageMode};

/// A real `GetProducts` answer for us-west-2 with [`FILTERS`] (recorded
/// 2026-11-16): the eight Fargate rows, Windows ones included.
const FIXTURE: &str = include_str!("fixtures/get-products-us-west-2.json");

fn fixture_items() -> Vec<String> {
    let v: Value = serde_json::from_str(FIXTURE).unwrap();
    v["PriceList"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap().to_string())
        .collect()
}

const X86: FargateRates = FargateRates {
    vcpu_hour: 0.04048,
    gb_hour: 0.004445,
    storage_gb_hour: 0.000111,
};
const ARM: FargateRates = FargateRates {
    vcpu_hour: 0.03238,
    gb_hour: 0.00356,
    storage_gb_hour: 0.000111,
};
const RATES: RegionRates = RegionRates {
    x86_64: Some(X86),
    arm64: Some(ARM),
};

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

fn cfg(cpu: u32, memory_mib: u32, ephemeral_gib: u32) -> AwsWorkspaceConfig {
    AwsWorkspaceConfig {
        cpu,
        memory_mib,
        ephemeral_gib,
        ..Default::default()
    }
}

// ---- estimate ----

#[test]
fn estimate_is_vcpus_plus_gbs_at_the_pinned_rates() {
    // 1 vCPU, 4 GiB, 21 GiB disk (1 billed).
    let e = estimate(&cfg(1024, 4096, 21), &X86);
    assert!(
        close(e.usd_per_hour, 0.04048 + 4.0 * 0.004445 + 0.000111),
        "{e:?}"
    );
    // 0.25 vCPU, 0.5 GiB, the free 20 GiB only.
    let e = estimate(&cfg(256, 512, 20), &X86);
    assert!(
        close(e.usd_per_hour, 0.25 * 0.04048 + 0.5 * 0.004445),
        "{e:?}"
    );
    // 4 vCPU, 16 GiB on ARM, 200 GiB disk (180 billed).
    let e = estimate(&cfg(4096, 16384, 200), &ARM);
    assert!(
        close(
            e.usd_per_hour,
            4.0 * 0.03238 + 16.0 * 0.00356 + 180.0 * 0.000111
        ),
        "{e:?}"
    );
}

#[test]
fn ephemeral_storage_is_billed_only_above_twenty_gib() {
    let free = estimate(&cfg(1024, 2048, 20), &X86).usd_per_hour;
    let one = estimate(&cfg(1024, 2048, 21), &X86).usd_per_hour;
    let fifty = estimate(&cfg(1024, 2048, 50), &X86).usd_per_hour;
    assert!(close(one - free, 0.000111));
    assert!(close(fifty - free, 30.0 * 0.000111));
}

#[test]
fn notes_follow_spot_and_storage_and_the_sidecar_adds_nothing() {
    let mut c = cfg(1024, 4096, 21);
    c.storage = StorageMode::Ephemeral;
    let base = estimate(&c, &X86);
    assert!(!base.note_spot && !base.efs_billed_separately);
    c.spot = true;
    assert!(estimate(&c, &X86).note_spot);
    for storage in [StorageMode::Hybrid, StorageMode::Efs] {
        c.storage = storage;
        assert!(estimate(&c, &X86).efs_billed_separately, "{storage:?}");
    }
    c.gateway_sidecar = true;
    assert!(close(estimate(&c, &X86).usd_per_hour, base.usd_per_hour));
}

#[test]
fn rates_are_picked_by_arch() {
    assert_eq!(RATES.for_arch(Arch::X86_64), Some(X86));
    assert_eq!(RATES.for_arch(Arch::Arm64), Some(ARM));
}

// ---- parsing ----

#[test]
fn the_recorded_price_list_parses_to_x86_and_arm_rates() {
    let rates = parse_price_list("us-west-2", &fixture_items()).expect("rates");
    assert_eq!(rates, RATES);
}

#[test]
fn a_price_list_for_another_region_gives_nothing() {
    assert_eq!(parse_price_list("eu-west-1", &fixture_items()), None);
}

#[test]
fn windows_rows_never_count() {
    let items: Vec<String> = fixture_items()
        .into_iter()
        .filter(|s| s.contains("Windows"))
        .collect();
    assert_eq!(items.len(), 3);
    assert_eq!(parse_price_list("us-west-2", &items), None);
}

#[test]
fn an_incomplete_architecture_is_none_and_storage_is_required() {
    let no_arm: Vec<String> = fixture_items()
        .into_iter()
        .filter(|s| !s.contains("Fargate-ARM-"))
        .collect();
    let rates = parse_price_list("us-west-2", &no_arm).unwrap();
    assert_eq!(rates.x86_64, Some(X86));
    assert_eq!(rates.arm64, None);

    let no_storage: Vec<String> = fixture_items()
        .into_iter()
        .filter(|s| !s.contains("EphemeralStorage"))
        .collect();
    assert_eq!(parse_price_list("us-west-2", &no_storage), None);
}

#[test]
fn garbage_and_bad_prices_are_skipped() {
    let mut items = vec!["not json".to_string(), "{}".to_string()];
    // A negative x86 vCPU price ahead of the real one is ignored.
    let bad = fixture_items()
        .into_iter()
        .find(|s| s.contains("USW2-Fargate-vCPU-Hours"))
        .unwrap()
        .replace("0.0404800000", "-1");
    items.push(bad);
    items.extend(fixture_items());
    assert_eq!(parse_price_list("us-west-2", &items), Some(RATES));
}

#[test]
fn the_filters_name_the_region_on_demand_and_fargate() {
    assert_eq!(PRICING_REGION, "us-east-1");
    assert_eq!(SERVICE_CODE, "AmazonECS");
    assert!(FILTERS.contains(&("TERM_MATCH", "regionCode", "{region}")));
    assert!(FILTERS.contains(&("TERM_MATCH", "termType", "OnDemand")));
    assert!(FILTERS.contains(&("CONTAINS", "usagetype", "Fargate")));
}

// ---- the API, faked ----

struct FakeApi {
    pages: Mutex<Vec<Result<Page, String>>>,
    calls: AtomicUsize,
    hang: bool,
}

impl FakeApi {
    fn answering(pages: Vec<Result<Page, String>>) -> Self {
        Self {
            pages: Mutex::new(pages),
            calls: AtomicUsize::new(0),
            hang: false,
        }
    }
    fn fixture() -> Self {
        Self::answering(vec![Ok(Page {
            items: fixture_items(),
            next_token: None,
        })])
    }
    fn denied() -> Self {
        Self::answering(vec![Err("Access denied: pricing:GetProducts".to_string())])
    }
    fn hanging() -> Self {
        Self {
            hang: true,
            ..Self::answering(vec![])
        }
    }
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl PricingApi for FakeApi {
    async fn get_products(&self, region: &str, _token: Option<String>) -> Result<Page, String> {
        assert_eq!(region, "us-west-2");
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.hang {
            std::future::pending::<()>().await;
        }
        let mut pages = self.pages.lock().unwrap();
        if pages.is_empty() {
            return Err("no more pages".to_string());
        }
        pages.remove(0)
    }
}

#[tokio::test]
async fn pages_are_joined() {
    let items = fixture_items();
    let (a, b) = items.split_at(4);
    let api = FakeApi::answering(vec![
        Ok(Page {
            items: a.to_vec(),
            next_token: Some("t".to_string()),
        }),
        Ok(Page {
            items: b.to_vec(),
            next_token: None,
        }),
    ]);
    assert_eq!(fetch_rates(&api, "us-west-2").await, Ok(RATES));
    assert_eq!(api.calls(), 2);
}

// ---- the cache ----

fn at(secs: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(secs)
}
const T0: u64 = 1_800_000_000;
const DAY: u64 = 24 * 60 * 60;

fn cache() -> (tempfile::TempDir, RatesCache) {
    let dir = tempfile::tempdir().unwrap();
    let cache = RatesCache::new(dir.path().join("aws-pricing"));
    (dir, cache)
}

#[test]
fn a_cache_file_is_fresh_for_a_day() {
    let (_d, c) = cache();
    assert_eq!(c.read("us-west-2", at(T0)), None);
    c.write("us-west-2", &RATES, at(T0)).unwrap();
    assert!(c.dir().join("us-west-2.json").is_file());
    assert_eq!(c.read("us-west-2", at(T0 + DAY - 1)), Some((RATES, true)));
    assert_eq!(c.read("us-west-2", at(T0 + DAY)), Some((RATES, false)));
    // From the future (the clock went back): stale.
    assert_eq!(c.read("us-west-2", at(T0 - 10)), Some((RATES, false)));
    // Another region's file is not this one's.
    assert_eq!(c.read("eu-west-1", at(T0)), None);
}

#[test]
fn a_corrupt_or_mislabelled_cache_file_is_no_cache() {
    let (_d, c) = cache();
    std::fs::create_dir_all(c.dir()).unwrap();
    std::fs::write(c.dir().join("us-west-2.json"), "{ not json").unwrap();
    assert_eq!(c.read("us-west-2", at(T0)), None);
    c.write("eu-west-1", &RATES, at(T0)).unwrap();
    std::fs::copy(
        c.dir().join("eu-west-1.json"),
        c.dir().join("us-west-2.json"),
    )
    .unwrap();
    assert_eq!(c.read("us-west-2", at(T0)), None);
}

#[tokio::test]
async fn a_fresh_cache_answers_without_a_call() {
    let (_d, c) = cache();
    c.write("us-west-2", &RATES, at(T0)).unwrap();
    let api = FakeApi::denied();
    assert_eq!(
        c.rates(Some(&api), "us-west-2", at(T0 + 60)).await,
        Some(RATES)
    );
    assert_eq!(api.calls(), 0);
}

#[tokio::test]
async fn a_miss_fetches_and_writes_the_cache() {
    let (_d, c) = cache();
    let api = FakeApi::fixture();
    assert_eq!(c.rates(Some(&api), "us-west-2", at(T0)).await, Some(RATES));
    assert_eq!(api.calls(), 1);
    assert_eq!(c.read("us-west-2", at(T0 + 1)), Some((RATES, true)));
}

#[tokio::test]
async fn an_expired_cache_is_fetched_again() {
    let (_d, c) = cache();
    let old = RegionRates {
        x86_64: Some(FargateRates {
            vcpu_hour: 1.0,
            ..X86
        }),
        arm64: None,
    };
    c.write("us-west-2", &old, at(T0)).unwrap();
    let api = FakeApi::fixture();
    assert_eq!(
        c.rates(Some(&api), "us-west-2", at(T0 + DAY)).await,
        Some(RATES)
    );
    assert_eq!(api.calls(), 1);
    assert_eq!(c.read("us-west-2", at(T0 + DAY)), Some((RATES, true)));
}

#[tokio::test]
async fn a_corrupt_cache_is_fetched_again() {
    let (_d, c) = cache();
    std::fs::create_dir_all(c.dir()).unwrap();
    std::fs::write(c.dir().join("us-west-2.json"), "garbage").unwrap();
    let api = FakeApi::fixture();
    assert_eq!(c.rates(Some(&api), "us-west-2", at(T0)).await, Some(RATES));
    assert_eq!(c.read("us-west-2", at(T0)), Some((RATES, true)));
}

#[tokio::test]
async fn denied_is_none_and_is_not_asked_again_for_a_while() {
    let (_d, c) = cache();
    let api = FakeApi::denied();
    assert_eq!(c.rates(Some(&api), "us-west-2", at(T0)).await, None);
    assert_eq!(c.rates(Some(&api), "us-west-2", at(T0 + 60)).await, None);
    assert_eq!(api.calls(), 1, "a recent failure is not retried");
    let later = T0 + RETRY_AFTER_FAILURE.as_secs();
    assert_eq!(c.rates(Some(&api), "us-west-2", at(later)).await, None);
    assert_eq!(api.calls(), 2);
    assert!(!c.dir().join("us-west-2.json").exists());
}

#[tokio::test]
async fn a_failed_refresh_falls_back_to_the_stale_cache() {
    let (_d, c) = cache();
    c.write("us-west-2", &RATES, at(T0)).unwrap();
    let api = FakeApi::denied();
    assert_eq!(
        c.rates(Some(&api), "us-west-2", at(T0 + 2 * DAY)).await,
        Some(RATES)
    );
    assert_eq!(api.calls(), 1);
    // No client at all (e.g. the connection's keys are gone): stale still answers.
    assert_eq!(
        c.rates(None::<&FakeApi>, "us-west-2", at(T0 + 2 * DAY))
            .await,
        Some(RATES)
    );
}

#[tokio::test(start_paused = true)]
async fn a_hanging_api_times_out_to_none() {
    let (_d, c) = cache();
    let api = FakeApi::hanging();
    let started = tokio::time::Instant::now();
    assert_eq!(c.rates(Some(&api), "us-west-2", at(T0)).await, None);
    assert_eq!(started.elapsed(), FETCH_TIMEOUT);
    assert_eq!(api.calls(), 1);
}

#[tokio::test(start_paused = true)]
async fn building_the_client_counts_against_the_same_timeout() {
    let (_d, c) = cache();
    c.write("us-west-2", &RATES, at(T0)).unwrap();
    let started = tokio::time::Instant::now();
    // The client never gets built (credentials that hang): stale answers at
    // the fetch's own bound, not later.
    let r = c
        .rates_with::<FakeApi, FakeApi>(std::future::pending(), "us-west-2", at(T0 + 2 * DAY))
        .await;
    assert_eq!(r, Some(RATES));
    assert_eq!(started.elapsed(), FETCH_TIMEOUT);

    // A fresh cache never builds a client at all.
    let fresh = c
        .rates_with::<FakeApi, FakeApi>(
            async { panic!("no client is built for a fresh cache") },
            "us-west-2",
            at(T0 + 60),
        )
        .await;
    assert_eq!(fresh, Some(RATES));
}

#[tokio::test]
async fn no_client_counts_as_a_failure_and_is_not_asked_again_for_a_while() {
    let (_d, c) = cache();
    let none = || async { None::<FakeApi> };
    assert_eq!(c.rates_with::<FakeApi, FakeApi>(none(), "us-west-2", at(T0)).await, None);
    // Within the back-off the client is not even built.
    let r = c
        .rates_with::<FakeApi, FakeApi>(
            async { panic!("no client is built within the back-off") },
            "us-west-2",
            at(T0 + 60),
        )
        .await;
    assert_eq!(r, None);
    // After it, a working client is asked and clears the failure.
    let api = FakeApi::fixture();
    let later = T0 + RETRY_AFTER_FAILURE.as_secs();
    assert_eq!(c.rates(Some(&api), "us-west-2", at(later)).await, Some(RATES));
    assert_eq!(api.calls(), 1);
}
