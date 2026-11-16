//! The estimated hourly cost of a Fargate workspace (Q5): on-demand rates
//! from the Price List API (`pricing:GetProducts`, service `AmazonECS`,
//! served from us-east-1 only), cached on disk per region for a day. An EC2
//! workspace (phase 7 E10) is priced as its instance: the `AmazonEC2`
//! on-demand Linux, shared-tenancy price of the user's instance type, cached
//! per region and type the same way.
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

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::models::project::{Arch, AwsWorkspaceConfig, StorageMode};

/// The Price List API's endpoint region (it also has ap-south-1 and
/// eu-central-1; any of them serves every region's prices).
pub const PRICING_REGION: &str = "us-east-1";
pub const SERVICE_CODE: &str = "AmazonECS";
/// EC2 instances' prices (phase 7 E10).
pub const EC2_SERVICE_CODE: &str = "AmazonEC2";
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

/// `GetProducts` filters for one EC2 instance type's on-demand Linux price
/// (`{region}`, `{instance_type}` filled in). Verified against the live API
/// (2026-11-16): exactly one row per type and region (`USW2-BoxUsage:m7i.large`,
/// $0.1008/hour), carrying both its OnDemand and Reserved terms.
pub const EC2_FILTERS: &[(&str, &str, &str)] = &[
    ("TERM_MATCH", "regionCode", "{region}"),
    ("TERM_MATCH", "instanceType", "{instance_type}"),
    ("TERM_MATCH", "operatingSystem", "Linux"),
    ("TERM_MATCH", "tenancy", "Shared"),
    ("TERM_MATCH", "preInstalledSw", "NA"),
    ("TERM_MATCH", "capacitystatus", "Used"),
    ("TERM_MATCH", "licenseModel", "No License required"),
];

/// One `GetProducts` query: a service and its filters, values filled in.
#[derive(Debug, Clone, PartialEq)]
pub struct Query {
    pub service_code: &'static str,
    pub filters: Vec<(&'static str, &'static str, String)>,
}

fn fill(filters: &[(&'static str, &'static str, &str)], vars: &[(&str, &str)]) -> Vec<(&'static str, &'static str, String)> {
    filters
        .iter()
        .map(|(kind, field, value)| {
            let value = vars
                .iter()
                .fold(value.to_string(), |v, (name, with)| v.replace(&format!("{{{name}}}"), with));
            (*kind, *field, value)
        })
        .collect()
}

/// The Fargate rates of `region` ([`FILTERS`]).
pub fn fargate_query(region: &str) -> Query {
    Query {
        service_code: SERVICE_CODE,
        filters: fill(FILTERS, &[("region", region)]),
    }
}

/// The on-demand price of `instance_type` in `region` ([`EC2_FILTERS`]).
pub fn ec2_query(region: &str, instance_type: &str) -> Query {
    Query {
        service_code: EC2_SERVICE_CODE,
        filters: fill(EC2_FILTERS, &[("region", region), ("instance_type", instance_type)]),
    }
}

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
/// (`efs_billed_separately`). `ec2_instance`: the figure is the user's EC2
/// instance's hourly price, whatever the project reserves of it, and
/// instances scale to zero when no workspace is running (E10).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct HourlyEstimate {
    pub usd_per_hour: f64,
    pub note_spot: bool,
    pub efs_billed_separately: bool,
    pub ec2_instance: bool,
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
        ec2_instance: false,
    }
}

/// An EC2 project's estimate: its instance's on-demand hourly price — the
/// instance is billed whole while it runs, so the reservations, the
/// ephemeral size and Spot (none on EC2) change nothing.
pub fn estimate_ec2(cfg: &AwsWorkspaceConfig, instance_usd_per_hour: f64) -> HourlyEstimate {
    HourlyEstimate {
        usd_per_hour: instance_usd_per_hour,
        note_spot: false,
        efs_billed_separately: cfg.storage != StorageMode::Ephemeral,
        ec2_instance: true,
    }
}

/// The on-demand hourly price of `instance_type` (Linux, shared tenancy) in
/// an `AmazonEC2` price list for `region`. The filters already narrow it to
/// one row; the attributes are checked again so a looser answer can't price
/// the wrong product. `None` when no row matches.
pub fn parse_ec2_price(region: &str, instance_type: &str, items: &[String]) -> Option<f64> {
    items.iter().find_map(|item| {
        let product = serde_json::from_str::<Value>(item).ok()?;
        let attrs = &product["product"]["attributes"];
        let is = |k: &str, v: &str| attrs[k].as_str() == Some(v);
        let matches = is("regionCode", region)
            && is("instanceType", instance_type)
            && is("operatingSystem", "Linux")
            && is("tenancy", "Shared")
            && is("preInstalledSw", "NA")
            && is("capacitystatus", "Used")
            && is("licenseModel", "No License required");
        if !matches {
            return None;
        }
        on_demand_usd(&product).filter(|p| *p > 0.0)
    })
}

/// Whether `instance_type` is safe in a cache file name and a filter value
/// (`m7i.large`): lowercase letters, digits, one dot.
fn plain_instance_type(instance_type: &str) -> bool {
    instance_type.split_once('.').is_some_and(|(family, size)| {
        let ok = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
        ok(family) && ok(size)
    }) && instance_type.len() <= 32
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
    /// One page of the products `query` matches.
    fn get_products(
        &self,
        query: &Query,
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
    async fn get_products(&self, query: &Query, next_token: Option<String>) -> Result<Page, String> {
        use aws_sdk_pricing::types::{Filter, FilterType};
        let mut req = self
            .client
            .get_products()
            .service_code(query.service_code)
            .format_version("aws_v1")
            .max_results(100)
            .set_next_token(next_token);
        for (kind, field, value) in &query.filters {
            let filter = Filter::builder()
                .r#type(FilterType::from(*kind))
                .field(*field)
                .value(value)
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

/// Every page `query` matches.
async fn fetch_items<A: PricingApi>(api: &A, query: &Query) -> Result<Vec<String>, String> {
    let mut items = Vec::new();
    let mut token = None;
    for _ in 0..MAX_PAGES {
        let page = api.get_products(query, token).await?;
        items.extend(page.items);
        token = page.next_token.filter(|t| !t.is_empty());
        if token.is_none() {
            return Ok(items);
        }
    }
    Err(format!("the {} price list has more than {MAX_PAGES} pages", query.service_code))
}

/// Every page for `region`, parsed (the live tests' direct call).
#[cfg_attr(not(test), allow(dead_code))]
pub async fn fetch_rates<A: PricingApi>(api: &A, region: &str) -> Result<RegionRates, String> {
    let items = fetch_items(api, &fargate_query(region)).await?;
    parse_price_list(region, &items).ok_or_else(|| format!("no Fargate rates for {region} in the price list"))
}

/// The on-demand hourly price of `instance_type` in `region` (the live
/// tests' direct call).
#[cfg_attr(not(test), allow(dead_code))]
pub async fn fetch_ec2_price<A: PricingApi>(api: &A, region: &str, instance_type: &str) -> Result<f64, String> {
    if !plain_instance_type(instance_type) {
        return Err(format!("{instance_type:?} is not an instance type"));
    }
    let items = fetch_items(api, &ec2_query(region, instance_type)).await?;
    parse_ec2_price(region, instance_type, &items)
        .ok_or_else(|| format!("no {instance_type} price for {region} in the price list"))
}

/// One cached answer: Fargate rates under `<region>.json`, an instance's
/// price under `<region>--ec2-<type>.json`. `key` is what the file must say
/// it holds (the region, or `<region> <type>`), and what failures are
/// remembered under.
struct CacheKey {
    key: String,
    file: String,
}

impl CacheKey {
    fn fargate(region: &str) -> Self {
        Self {
            key: region.to_string(),
            file: format!("{region}.json"),
        }
    }
    fn ec2(region: &str, instance_type: &str) -> Self {
        Self {
            key: format!("{region} {instance_type}"),
            file: format!("{region}--ec2-{instance_type}.json"),
        }
    }
}

/// A cache file. `key` was `region` before EC2 prices were cached too (an
/// older file still reads).
#[derive(Debug, Serialize, Deserialize)]
struct CacheFile<V> {
    #[serde(alias = "region")]
    key: String,
    /// Unix seconds.
    fetched_at: u64,
    rates: V,
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

    /// The cached rates and whether they are still fresh at `now`. A
    /// missing, unreadable or corrupt file, or one for another region, is
    /// no cache. The region must already be validated (it names a file).
    pub fn read(&self, region: &str, now: SystemTime) -> Option<(RegionRates, bool)> {
        self.read_key(&CacheKey::fargate(region), now)
    }

    #[cfg(test)]
    pub fn write(&self, region: &str, rates: &RegionRates, now: SystemTime) -> Result<(), String> {
        self.write_key(&CacheKey::fargate(region), rates, now)
    }

    /// [`Self::read`] for an instance type's hourly price.
    pub fn read_ec2(&self, region: &str, instance_type: &str, now: SystemTime) -> Option<(f64, bool)> {
        if !plain_instance_type(instance_type) {
            return None;
        }
        self.read_key(&CacheKey::ec2(region, instance_type), now)
    }

    #[cfg(test)]
    pub fn write_ec2(&self, region: &str, instance_type: &str, usd_per_hour: f64, now: SystemTime) -> Result<(), String> {
        if !plain_instance_type(instance_type) {
            return Err(format!("{instance_type:?} is not an instance type"));
        }
        self.write_key(&CacheKey::ec2(region, instance_type), &usd_per_hour, now)
    }

    fn read_key<V: DeserializeOwned>(&self, key: &CacheKey, now: SystemTime) -> Option<(V, bool)> {
        let text = std::fs::read_to_string(self.dir.join(&key.file)).ok()?;
        let file: CacheFile<V> = serde_json::from_str(&text).ok()?;
        if file.key != key.key {
            return None;
        }
        let now = unix_secs(now);
        // A timestamp from the future (a clock change) counts as stale.
        let fresh = file.fetched_at <= now && now - file.fetched_at < CACHE_TTL.as_secs();
        Some((file.rates, fresh))
    }

    fn write_key<V: Serialize>(&self, key: &CacheKey, rates: &V, now: SystemTime) -> Result<(), String> {
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        let file = CacheFile {
            key: key.key.clone(),
            fetched_at: unix_secs(now),
            rates,
        };
        let text = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
        // Write then rename, so a concurrent reader never sees half a file.
        let tmp = self
            .dir
            .join(format!(".{}.tmp-{}", key.file, std::process::id()));
        std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, self.dir.join(&key.file)).map_err(|e| {
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
        self.rates_with::<A, _>(async { api }, region, now).await
    }

    /// [`Self::rates`] with the client built by `api`, which is awaited only
    /// when the cache can't answer, and within the same [`FETCH_TIMEOUT`] as
    /// the fetch (building one reads credentials, which can hang too).
    pub async fn rates_with<A: PricingApi, B: std::borrow::Borrow<A>>(
        &self,
        api: impl Future<Output = Option<B>>,
        region: &str,
        now: SystemTime,
    ) -> Option<RegionRates> {
        let parse = |items: &[String]| {
            parse_price_list(region, items).ok_or_else(|| format!("no Fargate rates for {region} in the price list"))
        };
        self.cached::<A, B, _>(&CacheKey::fargate(region), api, fargate_query(region), parse, now)
            .await
    }

    /// [`Self::rates_with`] for `instance_type`'s on-demand hourly price in
    /// `region` (E10): the same cache rules, failure back-off and timeout.
    pub async fn ec2_price_with<A: PricingApi, B: std::borrow::Borrow<A>>(
        &self,
        api: impl Future<Output = Option<B>>,
        region: &str,
        instance_type: &str,
        now: SystemTime,
    ) -> Option<f64> {
        if !plain_instance_type(instance_type) {
            return None;
        }
        let parse = |items: &[String]| {
            parse_ec2_price(region, instance_type, items)
                .ok_or_else(|| format!("no {instance_type} price for {region} in the price list"))
        };
        let query = ec2_query(region, instance_type);
        self.cached::<A, B, _>(&CacheKey::ec2(region, instance_type), api, query, parse, now)
            .await
    }

    /// Fresh cache, else a fetch of `query` (client build included, within
    /// [`FETCH_TIMEOUT`]) parsed by `parse` that refreshes the cache, else
    /// the stale cache, else `None`; a key whose fetch failed is not asked
    /// again for [`RETRY_AFTER_FAILURE`].
    async fn cached<A: PricingApi, B: std::borrow::Borrow<A>, V: Serialize + DeserializeOwned + Clone>(
        &self,
        key: &CacheKey,
        api: impl Future<Output = Option<B>>,
        query: Query,
        parse: impl FnOnce(&[String]) -> Result<V, String>,
        now: SystemTime,
    ) -> Option<V> {
        let cached = self.read_key::<V>(key, now);
        let stale = match cached {
            Some((rates, true)) => return Some(rates),
            Some((rates, false)) => Some(rates),
            None => None,
        };
        let what = &key.key;
        if self.recently_failed(what, now) {
            return stale;
        }
        let fetch = async {
            let api = api.await?;
            Some(fetch_items(api.borrow(), &query).await.and_then(|items| parse(&items)))
        };
        let fetched = match tokio::time::timeout(FETCH_TIMEOUT, fetch).await {
            Ok(Some(r)) => r,
            // No client could be built: a failure like any other (building
            // one reads credentials, which can be slow), and the stale cache
            // answers.
            Ok(None) => {
                log::info!("No prices for {what}: no Pricing client could be built");
                self.note_failure(what, now, true);
                return stale;
            }
            Err(_) => Err(format!("timed out after {}s", FETCH_TIMEOUT.as_secs())),
        };
        match fetched {
            Ok(rates) => {
                self.note_failure(what, now, false);
                if let Err(e) = self.write_key(key, &rates, now) {
                    log::warn!("Could not cache the prices for {what}: {e}");
                }
                Some(rates)
            }
            Err(e) => {
                log::info!("No prices for {what}: {e}");
                self.note_failure(what, now, true);
                stale
            }
        }
    }
}

#[cfg(test)]
mod tests;
