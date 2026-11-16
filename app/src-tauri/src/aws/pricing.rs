//! The estimated hourly cost of a Fargate workspace (Q5): on-demand rates
//! from the Price List API (`pricing:GetProducts`, service `AmazonECS`,
//! served from us-east-1 only), cached on disk per region for a day.
//!
//! Everything here is best effort. A denied call, no network, a slow answer
//! or a price list we can't read all end in "no estimate" (`None`), never an
//! error: the estimate is a hint next to the settings, not something a start
//! depends on.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::models::project::{Arch, AwsWorkspaceConfig, StorageMode};

/// The Price List API's endpoint region (it also has ap-south-1 and
/// eu-central-1; any of them serves every region's prices).
pub const PRICING_REGION: &str = "us-east-1";
pub const SERVICE_CODE: &str = "AmazonECS";
/// How long a region's rates are used before they are fetched again.
pub const CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// The whole fetch (every page), so the command answers quickly offline.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(8);
/// After a failed fetch, the region is not asked again for this long (the
/// settings call the estimate on every change).
pub const RETRY_AFTER_FAILURE: Duration = Duration::from_secs(5 * 60);
/// Fargate includes 20 GiB of ephemeral storage; only the rest is billed.
pub const FREE_EPHEMERAL_GIB: u32 = 20;
/// The filtered list is 8 rows (one page); more pages than this is not a
/// price list we understand.
const MAX_PAGES: usize = 5;

/// `GetProducts` filters as (type, field, value); `{region}` is the
/// workspace's region. Verified against the live API (2026-11-16): eight
/// rows per region — Linux x86 and ARM vCPU-hours and GB-hours, ephemeral
/// storage GB-hours and three Windows rows, which the parser skips.
pub const FILTERS: &[(&str, &str, &str)] = &[
    ("TERM_MATCH", "regionCode", "{region}"),
    ("TERM_MATCH", "termType", "OnDemand"),
    ("CONTAINS", "usagetype", "Fargate"),
];

/// One architecture's on-demand rates in a region, in USD.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FargateRates {
    pub vcpu_hour: f64,
    pub gb_hour: f64,
    pub storage_gb_hour: f64,
}

/// A region's rates per architecture. An architecture the price list did
/// not fully cover is `None`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RegionRates {
    pub x86_64: Option<FargateRates>,
    pub arm64: Option<FargateRates>,
}

impl RegionRates {
    pub fn for_arch(&self, arch: Arch) -> Option<FargateRates> {
        match arch {
            Arch::X86_64 => self.x86_64,
            Arch::Arm64 => self.arm64,
        }
    }
}

/// What the UI shows: the on-demand price per hour of the task while it
/// runs. A gateway sidecar shares the task's CPU and memory, so it adds
/// nothing; Spot isn't in the Price List API (`note_spot`: say it is
/// usually cheaper); EFS is billed per GB-month, not per hour
/// (`efs_billed_separately`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct HourlyEstimate {
    pub usd_per_hour: f64,
    pub note_spot: bool,
    pub efs_billed_separately: bool,
}

pub fn estimate(cfg: &AwsWorkspaceConfig, rates: &FargateRates) -> HourlyEstimate {
    let vcpu = f64::from(cfg.cpu) / 1024.0;
    let gb = f64::from(cfg.memory_mib) / 1024.0;
    let billed_storage = f64::from(cfg.ephemeral_gib.saturating_sub(FREE_EPHEMERAL_GIB));
    HourlyEstimate {
        usd_per_hour: vcpu * rates.vcpu_hour
            + gb * rates.gb_hour
            + billed_storage * rates.storage_gb_hour,
        note_spot: cfg.spot,
        efs_billed_separately: cfg.storage != StorageMode::Ephemeral,
    }
}

/// The rates in a `GetProducts` price list (each item one product's JSON)
/// for `region`. `None` when no architecture is complete.
pub fn parse_price_list(region: &str, items: &[String]) -> Option<RegionRates> {
    #[derive(Default)]
    struct Partial {
        vcpu: Option<f64>,
        gb: Option<f64>,
    }
    let (mut x86, mut arm, mut storage) = (Partial::default(), Partial::default(), None);
    for item in items {
        let Ok(product) = serde_json::from_str::<Value>(item) else {
            continue;
        };
        let attrs = &product["product"]["attributes"];
        if attrs["regionCode"].as_str() != Some(region) {
            continue;
        }
        if attrs["operatingSystem"]
            .as_str()
            .is_some_and(|os| os != "Linux")
        {
            continue;
        }
        let Some(usage) = attrs["usagetype"].as_str() else {
            continue;
        };
        // `USW2-Fargate-ARM-vCPU-Hours:perCPU`; us-east-1's prefix is
        // `USE1-`, Ireland's `EU-`.
        let Some((_, kind)) = usage.split_once("Fargate-") else {
            continue;
        };
        let slot = match kind {
            "vCPU-Hours:perCPU" => &mut x86.vcpu,
            "GB-Hours" => &mut x86.gb,
            "ARM-vCPU-Hours:perCPU" => &mut arm.vcpu,
            "ARM-GB-Hours" => &mut arm.gb,
            "EphemeralStorage-GB-Hours" => &mut storage,
            _ => continue,
        };
        if slot.is_none() {
            *slot = on_demand_usd(&product);
        }
    }
    let storage = storage?;
    let complete = |p: Partial| {
        Some(FargateRates {
            vcpu_hour: p.vcpu?,
            gb_hour: p.gb?,
            storage_gb_hour: storage,
        })
    };
    let rates = RegionRates {
        x86_64: complete(x86),
        arm64: complete(arm),
    };
    (rates.x86_64.is_some() || rates.arm64.is_some()).then_some(rates)
}

/// The product's on-demand USD price per unit. A price list gives one term
/// with one dimension for these rows; anything not a sane non-negative
/// number is ignored.
fn on_demand_usd(product: &Value) -> Option<f64> {
    product["terms"]["OnDemand"]
        .as_object()?
        .values()
        .filter_map(|term| term["priceDimensions"].as_object())
        .flat_map(|dims| dims.values())
        .filter_map(|d| d["pricePerUnit"]["USD"].as_str()?.parse::<f64>().ok())
        .find(|p| p.is_finite() && (0.0..100.0).contains(p))
}

/// One page of `GetProducts`.
pub struct Page {
    pub items: Vec<String>,
    pub next_token: Option<String>,
}

/// The Price List API, faked in tests.
pub trait PricingApi {
    /// One page of `AmazonECS` products matching [`FILTERS`] for `region`.
    fn get_products(
        &self,
        region: &str,
        next_token: Option<String>,
    ) -> impl Future<Output = Result<Page, String>> + Send;
}

/// The real thing: a client in [`PRICING_REGION`] from a connection's
/// `SdkConfig` (`aws::sdk_config(conn, PRICING_REGION)`).
pub struct SdkPricingApi {
    client: aws_sdk_pricing::Client,
}

impl SdkPricingApi {
    pub fn new(cfg: &aws_config::SdkConfig) -> Self {
        Self {
            client: aws_sdk_pricing::Client::new(cfg),
        }
    }
}

impl PricingApi for SdkPricingApi {
    async fn get_products(&self, region: &str, next_token: Option<String>) -> Result<Page, String> {
        use aws_sdk_pricing::types::{Filter, FilterType};
        let mut req = self
            .client
            .get_products()
            .service_code(SERVICE_CODE)
            .format_version("aws_v1")
            .max_results(100)
            .set_next_token(next_token);
        for (kind, field, value) in FILTERS {
            let filter = Filter::builder()
                .r#type(FilterType::from(*kind))
                .field(*field)
                .value(value.replace("{region}", region))
                .build()
                .map_err(|e| e.to_string())?;
            req = req.filters(filter);
        }
        let out = req
            .send()
            .await
            .map_err(|e| super::classify_sdk_error("pricing:GetProducts", &e).to_string())?;
        Ok(Page {
            items: out.price_list().to_vec(),
            next_token: out.next_token().map(str::to_string),
        })
    }
}

/// Every page for `region`, parsed.
pub async fn fetch_rates<A: PricingApi>(api: &A, region: &str) -> Result<RegionRates, String> {
    let mut items = Vec::new();
    let mut token = None;
    for _ in 0..MAX_PAGES {
        let page = api.get_products(region, token).await?;
        items.extend(page.items);
        token = page.next_token.filter(|t| !t.is_empty());
        if token.is_none() {
            return parse_price_list(region, &items)
                .ok_or_else(|| format!("no Fargate rates for {region} in the price list"));
        }
    }
    Err(format!(
        "the {region} price list has more than {MAX_PAGES} pages"
    ))
}

/// `<dir>/<region>.json`.
#[derive(Debug, Serialize, Deserialize)]
struct CacheFile {
    region: String,
    /// Unix seconds.
    fetched_at: u64,
    rates: RegionRates,
}

/// The per-region rate files and the memory of recent failures.
pub struct RatesCache {
    dir: PathBuf,
    failed_at: Mutex<HashMap<String, SystemTime>>,
}

/// `<data dir>/triple-c/aws-pricing/`.
pub fn default_cache_dir() -> Option<PathBuf> {
    Some(dirs::data_dir()?.join("triple-c").join("aws-pricing"))
}

fn unix_secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

impl RatesCache {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            failed_at: Mutex::new(HashMap::new()),
        }
    }

    #[cfg(test)]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The region must already be validated (it names a file).
    fn path(&self, region: &str) -> PathBuf {
        self.dir.join(format!("{region}.json"))
    }

    /// The cached rates and whether they are still fresh at `now`. A
    /// missing, unreadable or corrupt file, or one for another region, is
    /// no cache.
    pub fn read(&self, region: &str, now: SystemTime) -> Option<(RegionRates, bool)> {
        let text = std::fs::read_to_string(self.path(region)).ok()?;
        let file: CacheFile = serde_json::from_str(&text).ok()?;
        if file.region != region {
            return None;
        }
        let now = unix_secs(now);
        // A timestamp from the future (a clock change) counts as stale.
        let fresh = file.fetched_at <= now && now - file.fetched_at < CACHE_TTL.as_secs();
        Some((file.rates, fresh))
    }

    pub fn write(&self, region: &str, rates: &RegionRates, now: SystemTime) -> Result<(), String> {
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        let file = CacheFile {
            region: region.to_string(),
            fetched_at: unix_secs(now),
            rates: *rates,
        };
        let text = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
        // Write then rename, so a concurrent reader never sees half a file.
        let tmp = self
            .dir
            .join(format!(".{region}.json.tmp-{}", std::process::id()));
        std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, self.path(region)).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            e.to_string()
        })
    }

    fn recently_failed(&self, region: &str, now: SystemTime) -> bool {
        let map = self.failed_at.lock().unwrap_or_else(|e| e.into_inner());
        map.get(region).is_some_and(|t| {
            now.duration_since(*t)
                .is_ok_and(|age| age < RETRY_AFTER_FAILURE)
        })
    }

    fn note_failure(&self, region: &str, now: SystemTime, failed: bool) {
        let mut map = self.failed_at.lock().unwrap_or_else(|e| e.into_inner());
        if failed {
            map.insert(region.to_string(), now);
        } else {
            map.remove(region);
        }
    }

    /// Fresh cached rates, else a fetch (bounded by [`FETCH_TIMEOUT`])
    /// that refreshes the cache, else the stale cache, else `None`. `api`
    /// is `None` when no client could be built (the stale cache still
    /// answers). A region whose fetch failed is not asked again for
    /// [`RETRY_AFTER_FAILURE`].
    pub async fn rates<A: PricingApi>(
        &self,
        api: Option<&A>,
        region: &str,
        now: SystemTime,
    ) -> Option<RegionRates> {
        let cached = self.read(region, now);
        if let Some((rates, true)) = cached {
            return Some(rates);
        }
        let stale = cached.map(|(rates, _)| rates);
        let Some(api) = api else {
            return stale;
        };
        if self.recently_failed(region, now) {
            return stale;
        }
        let fetched = match tokio::time::timeout(FETCH_TIMEOUT, fetch_rates(api, region)).await {
            Ok(r) => r,
            Err(_) => Err(format!("timed out after {}s", FETCH_TIMEOUT.as_secs())),
        };
        match fetched {
            Ok(rates) => {
                self.note_failure(region, now, false);
                if let Err(e) = self.write(region, &rates, now) {
                    log::warn!("Could not cache the {region} Fargate rates: {e}");
                }
                Some(rates)
            }
            Err(e) => {
                log::info!("No Fargate rates for {region}: {e}");
                self.note_failure(region, now, true);
                stale
            }
        }
    }
}

#[cfg(test)]
mod tests;
