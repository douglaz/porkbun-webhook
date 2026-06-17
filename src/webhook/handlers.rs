use super::types::*;
use crate::config::Config;
use crate::error::{Error, Result};
use crate::porkbun::{self, Client as PorkbunClient};
use axum::{
    Json,
    extract::Query,
    extract::rejection::JsonRejection,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;
use tracing::{debug, error, info};

/// Cached list of managed domains, used when DOMAIN_FILTER is unset.
struct DomainCache {
    domains: Vec<String>,
    expires_at: Instant,
}

pub struct WebhookHandler {
    porkbun_client: Arc<PorkbunClient>,
    config: Config,
    domain_cache: Mutex<Option<DomainCache>>,
}

impl WebhookHandler {
    pub fn new(porkbun_client: Arc<PorkbunClient>, config: Config) -> Self {
        Self {
            porkbun_client,
            config,
            domain_cache: Mutex::new(None),
        }
    }

    // ---------------------------------------------------------------
    // Endpoint handlers
    // ---------------------------------------------------------------

    pub async fn health(&self) -> Result<Json<HealthResponse>> {
        Ok(Json(HealthResponse {
            status: "healthy".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }))
    }

    pub async fn ready(&self) -> Result<Json<HealthResponse>> {
        self.porkbun_client.ping().await?;

        Ok(Json(HealthResponse {
            status: "ready".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }))
    }

    pub async fn negotiate(&self) -> Result<Response> {
        let filters = self.config.normalized_domain_filter();

        Ok((
            StatusCode::OK,
            [(
                "content-type",
                "application/external.dns.webhook+json;version=1",
            )],
            Json(serde_json::json!({
                "filters": filters
            })),
        )
            .into_response())
    }

    pub async fn get_records(&self, query: Query<GetRecordsQuery>) -> Result<Json<Vec<Endpoint>>> {
        if let Some(zone_name) = query.zone_name.as_ref() {
            let zone = Config::normalize_domain(zone_name);
            info!("Getting records for zone: {zone}");

            // Validate that the zone is an exact managed zone, not a subdomain.
            let managed = self.get_managed_domains().await?;
            if !managed.contains(&zone) {
                return Err(Error::DomainNotAllowed(format!(
                    "{zone} is not a managed zone"
                )));
            }

            let records = self.porkbun_client.list_records(&zone).await?;
            let endpoints = self.filter_and_group_records(&records, &zone);

            info!("Returning {} endpoints for zone {zone}", endpoints.len());
            Ok(Json(endpoints))
        } else {
            info!("Getting records for all configured domains");

            let domains = self.get_managed_domains().await?;
            let mut all_endpoints = Vec::new();
            let mut skipped_zones = 0u32;
            let mut succeeded_zones = 0u32;

            for domain in &domains {
                debug!("Fetching records for domain: {domain}");

                match self.porkbun_client.list_records(domain).await {
                    Ok(records) => {
                        let endpoints = self.filter_and_group_records(&records, domain);
                        debug!("Found {} endpoints for domain {domain}", endpoints.len());
                        all_endpoints.extend(endpoints);
                        succeeded_zones += 1;
                    }
                    Err(e) if e.is_transient() => {
                        // Transient error: fail the whole request so ExternalDNS retries.
                        error!("Transient failure fetching records for {domain}: {e}");
                        return Err(e);
                    }
                    Err(Error::PorkbunApi(msg)) => {
                        // Zone-specific API error — may not be truly zone-local.
                        error!("Skipping zone {domain}: {msg}");
                        skipped_zones += 1;
                    }
                    Err(Error::DomainNotAllowed(msg)) => {
                        error!("Skipping zone {domain}: not allowed ({msg})");
                        skipped_zones += 1;
                    }
                    Err(e) => {
                        // Auth, Json, Internal, etc. are global — fail the request.
                        error!("Fatal error fetching records for {domain}: {e}");
                        return Err(e);
                    }
                }
            }

            if skipped_zones > 0 {
                error!(
                    "Skipped {skipped_zones} of {} zones during record retrieval",
                    domains.len()
                );
            }

            // If no zones succeeded and we had domains to query, fail the request
            // to prevent ExternalDNS from reconciling against an empty provider state.
            if succeeded_zones == 0 && !domains.is_empty() {
                return Err(Error::PorkbunApi(format!(
                    "All {} zones failed during record retrieval ({skipped_zones} skipped)",
                    domains.len()
                )));
            }

            info!("Returning {} total endpoints", all_endpoints.len());
            Ok(Json(all_endpoints))
        }
    }

    pub async fn apply_changes(
        &self,
        body: std::result::Result<Json<ApplyChangesRequest>, JsonRejection>,
    ) -> Result<StatusCode> {
        let Json(request) = body.map_err(|e| Error::InvalidRequest(e.body_text()))?;
        let changes = request.into_changes();
        info!(
            "Applying changes: {} creates, {} updates, {} deletes",
            changes.create.len(),
            changes.update_new.len(),
            changes.delete.len()
        );

        if changes.is_empty() {
            info!("No changes to apply");
            return Ok(StatusCode::NO_CONTENT);
        }

        // updateOld and updateNew must be paired 1:1
        if changes.update_old.len() != changes.update_new.len() {
            return Err(Error::InvalidRequest(format!(
                "updateOld ({}) and updateNew ({}) must have the same length",
                changes.update_old.len(),
                changes.update_new.len()
            )));
        }

        // --- Validation phase: fail fast before any mutations ---
        let all_endpoints: Vec<&Endpoint> = changes
            .create
            .iter()
            .chain(changes.update_new.iter())
            .chain(changes.delete.iter())
            .collect();

        for ep in &all_endpoints {
            self.validate_endpoint(ep).await?;
        }
        // Also validate update_old endpoints
        for ep in &changes.update_old {
            self.validate_endpoint(ep).await?;
        }

        // --- Mutation phase ---
        // Order: deletes, updates, creates
        // Zone record cache: avoids re-fetching records for the same zone.
        let mut zone_cache: HashMap<String, Vec<porkbun::DnsRecord>> = HashMap::new();

        // Process deletions
        for endpoint in &changes.delete {
            info!(
                "DELETE: {} -> {}",
                endpoint.dns_name,
                endpoint.targets.join(", ")
            );
            self.delete_endpoint(endpoint, &mut zone_cache)
                .await
                .map_err(|e| {
                    error!("Failed to delete endpoint {}: {e}", endpoint.dns_name);
                    e
                })?;
        }

        // Process updates
        for (old, new) in changes.update_old.iter().zip(changes.update_new.iter()) {
            info!(
                "UPDATE: {} from {} to {}",
                new.dns_name,
                old.targets.join(", "),
                new.targets.join(", ")
            );
            self.update_endpoint(old, new, &mut zone_cache)
                .await
                .map_err(|e| {
                    error!("Failed to update endpoint {}: {e}", new.dns_name);
                    e
                })?;
        }

        // Process creations
        for endpoint in &changes.create {
            info!(
                "CREATE: {} -> {}",
                endpoint.dns_name,
                endpoint.targets.join(", ")
            );
            self.create_endpoint(endpoint, &mut zone_cache)
                .await
                .map_err(|e| {
                    error!("Failed to create endpoint {}: {e}", endpoint.dns_name);
                    e
                })?;
        }

        info!("Successfully applied all changes");
        Ok(StatusCode::NO_CONTENT)
    }

    pub async fn adjust_endpoints(
        &self,
        Json(endpoints): Json<Vec<Endpoint>>,
    ) -> Result<Json<Vec<Endpoint>>> {
        debug!("Adjusting {} endpoints", endpoints.len());
        Ok(Json(endpoints))
    }

    // ---------------------------------------------------------------
    // Record reconciliation helpers
    // ---------------------------------------------------------------

    /// Validate that an endpoint is well-formed and targets an allowed zone.
    async fn validate_endpoint(&self, endpoint: &Endpoint) -> Result<()> {
        // Supported record type
        if !matches!(
            endpoint.record_type.as_str(),
            "A" | "AAAA" | "CNAME" | "TXT" | "MX" | "SRV"
        ) {
            return Err(Error::InvalidRequest(format!(
                "Unsupported record type: {}",
                endpoint.record_type
            )));
        }

        // Targets must not be empty
        if endpoint.targets.is_empty() {
            return Err(Error::InvalidRequest(format!(
                "No targets for endpoint {}",
                endpoint.dns_name
            )));
        }

        // Each target must be non-blank
        for target in &endpoint.targets {
            if target.trim().is_empty() {
                return Err(Error::InvalidRequest(format!(
                    "Blank target in endpoint {}",
                    endpoint.dns_name
                )));
            }
        }

        // Zone must be resolvable and allowed
        let zone = self.resolve_zone(&endpoint.dns_name).await?;
        if !self.config.is_domain_allowed(&zone) {
            return Err(Error::DomainNotAllowed(zone));
        }

        // Validate priority if present
        endpoint_priority(endpoint)?;

        Ok(())
    }

    /// Create an endpoint idempotently. If records already exist with the
    /// same content, they are left alone.
    async fn create_endpoint(
        &self,
        endpoint: &Endpoint,
        zone_cache: &mut HashMap<String, Vec<porkbun::DnsRecord>>,
    ) -> Result<()> {
        let zone = self.resolve_zone(&endpoint.dns_name).await?;
        let subdomain = self.to_porkbun_subdomain(&endpoint.dns_name, &zone);
        let existing = self
            .list_matching_records(&zone, endpoint, zone_cache)
            .await?;
        let ttl = endpoint_ttl(endpoint);
        let prio = endpoint_priority(endpoint)?;

        // Deduplicate targets to prevent duplicate API calls when endpoint.targets
        // contains the same target more than once.
        let mut seen = std::collections::HashSet::new();
        let unique_targets: Vec<&String> = endpoint
            .targets
            .iter()
            .filter(|t| seen.insert(normalize_target(t, &endpoint.record_type)))
            .collect();

        for target in unique_targets {
            // Check if record already exists (normalize hostname targets for comparison)
            let already_exists = existing.iter().any(|r| {
                targets_match(&r.content, target, &endpoint.record_type)
                    && ttl_effectively_equal(r.ttl_u32(), ttl)
                    && prio_effectively_equal(r.prio_u32(), prio)
            });

            if already_exists {
                debug!(
                    "Record already exists: {} {} -> {target}",
                    endpoint.dns_name, endpoint.record_type
                );
                continue;
            }

            if self.config.dry_run {
                info!(
                    "DRY RUN: Would create {} {} -> {target} in zone {zone}",
                    endpoint.record_type, endpoint.dns_name
                );
                continue;
            }

            let params = porkbun::CreateDnsParams {
                subdomain: subdomain.clone(),
                record_type: endpoint.record_type.clone(),
                content: target.clone(),
                ttl,
                prio,
            };

            self.porkbun_client.add_record(&zone, params).await?;
            zone_cache.remove(&zone);
        }

        Ok(())
    }

    /// Delete an endpoint idempotently. Missing records are not errors.
    async fn delete_endpoint(
        &self,
        endpoint: &Endpoint,
        zone_cache: &mut HashMap<String, Vec<porkbun::DnsRecord>>,
    ) -> Result<()> {
        let zone = self.resolve_zone(&endpoint.dns_name).await?;
        let existing = self
            .list_matching_records(&zone, endpoint, zone_cache)
            .await?;

        for record in &existing {
            let content_matches = endpoint
                .targets
                .iter()
                .any(|t| targets_match(t, &record.content, &endpoint.record_type));
            if content_matches {
                if self.config.dry_run {
                    info!(
                        "DRY RUN: Would delete record {} ({} -> {})",
                        record.id, endpoint.dns_name, record.content
                    );
                    continue;
                }

                self.porkbun_client.remove_record(&zone, &record.id).await?;
                zone_cache.remove(&zone);
            }
        }

        Ok(())
    }

    /// Update an endpoint by computing a diff between old and new.
    async fn update_endpoint(
        &self,
        old: &Endpoint,
        new: &Endpoint,
        zone_cache: &mut HashMap<String, Vec<porkbun::DnsRecord>>,
    ) -> Result<()> {
        let old_zone = self.resolve_zone(&old.dns_name).await?;
        let new_zone = self.resolve_zone(&new.dns_name).await?;

        // If zone/name/type changed, do a full delete + create
        if old_zone != new_zone
            || normalize_fqdn(&old.dns_name) != normalize_fqdn(&new.dns_name)
            || old.record_type != new.record_type
        {
            self.delete_endpoint(old, zone_cache).await?;
            self.create_endpoint(new, zone_cache).await?;
            return Ok(());
        }

        let zone = &new_zone;
        let subdomain = self.to_porkbun_subdomain(&new.dns_name, zone);
        let existing = self.list_matching_records(zone, old, zone_cache).await?;
        let new_ttl = endpoint_ttl(new);
        let new_prio = endpoint_priority(new)?;

        let rtype = &new.record_type;

        // Targets to remove: in old but not new, deduplicated to prevent
        // redundant delete passes when old.targets contains duplicate entries.
        let mut seen_remove = std::collections::HashSet::new();
        let targets_to_remove: Vec<&String> = old
            .targets
            .iter()
            .filter(|t| !new.targets.iter().any(|nt| targets_match(nt, t, rtype)))
            .filter(|t| seen_remove.insert(normalize_target(t, rtype)))
            .collect();

        // Targets to add: in new but not old, deduplicated to prevent
        // duplicate API calls when new.targets contains the same target twice.
        let mut seen_add = std::collections::HashSet::new();
        let targets_to_add: Vec<&String> = new
            .targets
            .iter()
            .filter(|t| !old.targets.iter().any(|ot| targets_match(ot, t, rtype)))
            .filter(|t| seen_add.insert(normalize_target(t, rtype)))
            .collect();

        // Shared targets: check if TTL/priority changed -> edit, deduplicated
        // to prevent redundant edit API calls when new.targets has duplicates.
        let mut seen_shared = std::collections::HashSet::new();
        let shared_targets: Vec<&String> = new
            .targets
            .iter()
            .filter(|t| old.targets.iter().any(|ot| targets_match(ot, t, rtype)))
            .filter(|t| seen_shared.insert(normalize_target(t, rtype)))
            .collect();

        // Delete removed targets — delete ALL matching records, not just the first,
        // to handle duplicate records from prior manual edits or partial failures.
        let mut had_deletions = false;
        for target in &targets_to_remove {
            let matching: Vec<_> = existing
                .iter()
                .filter(|r| targets_match(&r.content, target, rtype))
                .collect();
            for record in matching {
                if self.config.dry_run {
                    info!(
                        "DRY RUN: Would delete record {} (target: {target})",
                        record.id
                    );
                    continue;
                }
                self.porkbun_client.remove_record(zone, &record.id).await?;
                zone_cache.remove(zone);
                had_deletions = true;
            }
        }

        // Re-fetch records after deletions so edits target current record IDs
        let existing = if had_deletions {
            self.list_matching_records(zone, old, zone_cache).await?
        } else {
            existing
        };

        // Edit shared targets if TTL/priority changed — edit ALL matching records,
        // not just the first, to handle duplicate records from prior manual edits
        // or partial failures. Stale duplicates with old TTL/priority would cause
        // group_porkbun_records to surface extra endpoints on the next read.
        for target in &shared_targets {
            let matching: Vec<_> = existing
                .iter()
                .filter(|r| targets_match(&r.content, target, rtype))
                .collect();
            for record in matching {
                let needs_edit = !ttl_effectively_equal(record.ttl_u32(), new_ttl)
                    || !prio_effectively_equal(record.prio_u32(), new_prio);

                if needs_edit {
                    if self.config.dry_run {
                        info!(
                            "DRY RUN: Would edit record {} (target: {target})",
                            record.id
                        );
                        continue;
                    }
                    let params = porkbun::EditDnsParams {
                        subdomain: subdomain.clone(),
                        record_type: new.record_type.clone(),
                        content: (*target).clone(),
                        ttl: new_ttl,
                        prio: new_prio,
                    };
                    self.porkbun_client
                        .edit_record(zone, &record.id, params)
                        .await?;
                    zone_cache.remove(zone);
                }
            }
        }

        // Create new targets (idempotent: check provider state first)
        // Re-fetch records after prior mutations to get current state
        if !targets_to_add.is_empty() {
            zone_cache.remove(zone);
            let current_records = self.list_matching_records(zone, new, zone_cache).await?;

            for target in &targets_to_add {
                let already_exists = current_records.iter().any(|r| {
                    targets_match(&r.content, target, rtype)
                        && ttl_effectively_equal(r.ttl_u32(), new_ttl)
                        && prio_effectively_equal(r.prio_u32(), new_prio)
                });

                if already_exists {
                    debug!(
                        "Record already exists during update: {} {} -> {target}",
                        new.dns_name, new.record_type
                    );
                    continue;
                }

                if self.config.dry_run {
                    info!("DRY RUN: Would create {} -> {target}", new.dns_name);
                    continue;
                }
                let params = porkbun::CreateDnsParams {
                    subdomain: subdomain.clone(),
                    record_type: new.record_type.clone(),
                    content: (*target).clone(),
                    ttl: new_ttl,
                    prio: new_prio,
                };
                self.porkbun_client.add_record(zone, params).await?;
                zone_cache.remove(zone);
            }
        }

        Ok(())
    }

    // ---------------------------------------------------------------
    // Zone resolution
    // ---------------------------------------------------------------

    /// Resolve the zone (domain) for a given FQDN using longest-suffix matching.
    /// Uses DOMAIN_FILTER if set, otherwise uses cached list_domains().
    async fn resolve_zone(&self, dns_name: &str) -> Result<String> {
        let normalized = normalize_fqdn(dns_name);

        if let Some(ref domains) = self.config.domain_filter {
            return longest_suffix_match(&normalized, domains).ok_or_else(|| {
                Error::DomainNotAllowed(format!("No configured zone matches {dns_name}"))
            });
        }

        // No filter: use cached domain list
        let domains = self.get_managed_domains().await?;
        longest_suffix_match(&normalized, &domains)
            .ok_or_else(|| Error::InvalidRequest(format!("No owned domain matches {dns_name}")))
    }

    /// Get the list of managed domains, using the cache when available.
    async fn get_managed_domains(&self) -> Result<Vec<String>> {
        if let Some(ref filter) = self.config.domain_filter {
            return Ok(filter.clone());
        }

        // Check cache
        {
            let cache = self.domain_cache.lock().await;
            if let Some(ref c) = *cache
                && Instant::now() < c.expires_at
            {
                return Ok(c.domains.clone());
            }
        }

        // Fetch fresh
        let api_domains = self.porkbun_client.list_domains().await?;
        let domains: Vec<String> = api_domains
            .iter()
            .map(|d| Config::normalize_domain(&d.domain))
            .collect();

        // Update cache
        {
            let mut cache = self.domain_cache.lock().await;
            *cache = Some(DomainCache {
                domains: domains.clone(),
                expires_at: Instant::now()
                    + std::time::Duration::from_secs(self.config.cache_ttl_seconds),
            });
        }

        Ok(domains)
    }

    // ---------------------------------------------------------------
    // Subdomain / record helpers
    // ---------------------------------------------------------------

    /// Convert a dns_name to the Porkbun subdomain field.
    /// Apex records use empty string; subdomains strip the zone suffix.
    fn to_porkbun_subdomain(&self, dns_name: &str, zone: &str) -> String {
        let normalized = normalize_fqdn(dns_name);
        if normalized == zone {
            String::new()
        } else if let Some(prefix) = normalized.strip_suffix(&format!(".{zone}")) {
            prefix.to_string()
        } else {
            // Shouldn't happen after resolve_zone, but be safe
            normalized
        }
    }

    /// List provider records matching an endpoint's FQDN and record type.
    /// Uses `zone_cache` to avoid re-fetching records for the same zone.
    async fn list_matching_records(
        &self,
        zone: &str,
        endpoint: &Endpoint,
        zone_cache: &mut HashMap<String, Vec<porkbun::DnsRecord>>,
    ) -> Result<Vec<porkbun::DnsRecord>> {
        let records = if let Some(cached) = zone_cache.get(zone) {
            cached.clone()
        } else {
            let fetched = self.porkbun_client.list_records(zone).await?;
            zone_cache.insert(zone.to_string(), fetched.clone());
            fetched
        };
        let target_fqdn = normalize_fqdn(&endpoint.dns_name);

        Ok(records
            .into_iter()
            .filter(|r| {
                let record_fqdn = normalize_fqdn(&r.name);
                // Apex: Porkbun returns zone name as the record name
                let fqdn_match = if record_fqdn.is_empty() || record_fqdn == zone {
                    target_fqdn == zone
                } else {
                    record_fqdn == target_fqdn
                };
                fqdn_match && r.record_type == endpoint.record_type
            })
            .collect())
    }

    /// Filter records to supported types and group into endpoints.
    fn filter_and_group_records(
        &self,
        records: &[porkbun::DnsRecord],
        zone: &str,
    ) -> Vec<Endpoint> {
        let supported: Vec<porkbun::DnsRecord> = records
            .iter()
            .filter(|r| {
                matches!(
                    r.record_type.as_str(),
                    "A" | "AAAA" | "CNAME" | "TXT" | "MX" | "SRV"
                )
            })
            .cloned()
            .collect();

        group_porkbun_records(&supported, zone)
    }
}

// ---------------------------------------------------------------
// Pure helper functions
// ---------------------------------------------------------------

/// Normalize a FQDN: lowercase, trim, strip trailing dot.
pub fn normalize_fqdn(name: &str) -> String {
    Config::normalize_domain(name)
}

/// Find the longest suffix match from a list of candidate zones.
fn longest_suffix_match(normalized_name: &str, candidates: &[String]) -> Option<String> {
    let mut best: Option<&String> = None;

    for candidate in candidates {
        let is_match =
            *normalized_name == *candidate || normalized_name.ends_with(&format!(".{candidate}"));
        if is_match {
            match best {
                Some(current) if candidate.len() <= current.len() => {}
                _ => best = Some(candidate),
            }
        }
    }

    best.cloned()
}

/// Extract priority from provider_specific metadata.
fn endpoint_priority(endpoint: &Endpoint) -> Result<Option<u32>> {
    match endpoint
        .provider_specific
        .iter()
        .find(|ps| ps.name == "priority")
    {
        Some(ps) => ps
            .value
            .parse::<u32>()
            .map(Some)
            .map_err(|_| Error::InvalidRequest(format!("Invalid priority value: {}", ps.value))),
        None => Ok(None),
    }
}

/// Extract TTL from endpoint. Zero, negative, or absent means None (use provider default).
/// Values exceeding u32::MAX are also treated as None.
fn endpoint_ttl(endpoint: &Endpoint) -> Option<u32> {
    endpoint
        .record_ttl
        .filter(|&ttl| ttl > 0 && ttl <= u32::MAX as i64)
        .map(|ttl| ttl as u32)
}

/// TTL comparison: treat None as "don't care" — only differ when both are set and different.
fn ttl_effectively_equal(existing: Option<u32>, desired: Option<u32>) -> bool {
    match (existing, desired) {
        (Some(a), Some(b)) => a == b,
        _ => true,
    }
}

/// Priority comparison: same as TTL.
fn prio_effectively_equal(existing: Option<u32>, desired: Option<u32>) -> bool {
    match (existing, desired) {
        (Some(a), Some(b)) => a == b,
        _ => true,
    }
}

/// Normalize a target value for comparison. For hostname-typed records (CNAME, MX, SRV),
/// lowercase and strip trailing dots to handle Porkbun/ExternalDNS formatting differences.
/// A and AAAA targets are IPs and TXT targets are opaque — leave them as-is.
fn normalize_target(target: &str, record_type: &str) -> String {
    match record_type {
        "CNAME" | "MX" | "SRV" => {
            let t = target.trim().to_ascii_lowercase();
            t.strip_suffix('.').unwrap_or(&t).to_string()
        }
        _ => target.to_string(),
    }
}

/// Compare two target values with type-aware normalization.
fn targets_match(a: &str, b: &str, record_type: &str) -> bool {
    normalize_target(a, record_type) == normalize_target(b, record_type)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn test_config() -> Config {
        Config {
            porkbun_api_key: "pk1_test".to_string(),
            porkbun_secret_api_key: "sk1_test".to_string(),
            porkbun_api_base: "http://localhost:1234".to_string(),
            webhook_host: "127.0.0.1".to_string(),
            webhook_port: 8888,
            domain_filter: Some(vec!["example.com".to_string()]),
            dry_run: true,
            cache_ttl_seconds: 60,
            http_timeout_seconds: 30,
            trace_request_bodies: false,
        }
    }

    fn test_handler() -> WebhookHandler {
        let config = test_config();
        let client = Arc::new(
            PorkbunClient::new(
                &config.porkbun_api_key,
                &config.porkbun_secret_api_key,
                &config.porkbun_api_base,
                std::time::Duration::from_secs(5),
            )
            .expect("client should build"),
        );
        WebhookHandler::new(client, config)
    }

    fn handler_with_filter(domains: Vec<&str>) -> WebhookHandler {
        let mut config = test_config();
        config.domain_filter = Some(domains.into_iter().map(Config::normalize_domain).collect());
        let client = Arc::new(
            PorkbunClient::new(
                &config.porkbun_api_key,
                &config.porkbun_secret_api_key,
                &config.porkbun_api_base,
                std::time::Duration::from_secs(5),
            )
            .expect("client should build"),
        );
        WebhookHandler::new(client, config)
    }

    // --- Zone resolution tests ---

    #[tokio::test]
    async fn resolve_zone_returns_canonical_zone() {
        let handler = test_handler();
        let zone = handler.resolve_zone("www.example.com").await.unwrap();
        assert_eq!(zone, "example.com");
    }

    #[tokio::test]
    async fn resolve_zone_exact_match() {
        let handler = test_handler();
        let zone = handler.resolve_zone("example.com").await.unwrap();
        assert_eq!(zone, "example.com");
    }

    #[tokio::test]
    async fn resolve_zone_deep_subdomain() {
        let handler = handler_with_filter(vec!["example.com"]);
        let zone = handler.resolve_zone("api.dev.example.com").await.unwrap();
        assert_eq!(zone, "example.com");
    }

    #[tokio::test]
    async fn resolve_zone_trailing_dot() {
        let handler = test_handler();
        let zone = handler.resolve_zone("foo.example.com.").await.unwrap();
        assert_eq!(zone, "example.com");
    }

    #[tokio::test]
    async fn resolve_zone_mixed_case() {
        let handler = handler_with_filter(vec!["Example.COM"]);
        let zone = handler.resolve_zone("www.example.com").await.unwrap();
        assert_eq!(zone, "example.com");
    }

    #[tokio::test]
    async fn resolve_zone_longest_suffix_wins() {
        let handler = handler_with_filter(vec!["co.uk", "example.co.uk"]);
        let zone = handler.resolve_zone("foo.example.co.uk").await.unwrap();
        assert_eq!(zone, "example.co.uk");
    }

    #[tokio::test]
    async fn resolve_zone_no_match_returns_error() {
        let handler = handler_with_filter(vec!["example.com"]);
        let result = handler.resolve_zone("unknown.org").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn resolve_zone_no_two_label_fallback() {
        // With filter set, unmatched names should NOT fall back to last-two-labels
        let handler = handler_with_filter(vec!["other.com"]);
        let result = handler.resolve_zone("www.fallback.org").await;
        assert!(
            result.is_err(),
            "should not fall back to two-label extraction"
        );
    }

    // --- Subdomain extraction tests ---

    #[test]
    fn to_porkbun_subdomain_apex() {
        let handler = test_handler();
        assert_eq!(
            handler.to_porkbun_subdomain("example.com", "example.com"),
            ""
        );
    }

    #[test]
    fn to_porkbun_subdomain_simple() {
        let handler = test_handler();
        assert_eq!(
            handler.to_porkbun_subdomain("www.example.com", "example.com"),
            "www"
        );
    }

    #[test]
    fn to_porkbun_subdomain_deep() {
        let handler = test_handler();
        assert_eq!(
            handler.to_porkbun_subdomain("api.dev.example.com", "example.com"),
            "api.dev"
        );
    }

    #[test]
    fn to_porkbun_subdomain_trailing_dot() {
        let handler = test_handler();
        assert_eq!(
            handler.to_porkbun_subdomain("www.example.com.", "example.com"),
            "www"
        );
    }

    #[test]
    fn to_porkbun_subdomain_mixed_case() {
        let handler = test_handler();
        assert_eq!(
            handler.to_porkbun_subdomain("WWW.Example.COM", "example.com"),
            "www"
        );
    }

    // --- Priority/TTL helper tests ---

    #[test]
    fn endpoint_priority_valid() {
        let ep = Endpoint {
            dns_name: "example.com".to_string(),
            targets: vec!["mail.example.com".to_string()],
            record_type: "MX".to_string(),
            set_identifier: None,
            record_ttl: None,
            labels: Default::default(),
            provider_specific: vec![ProviderSpecific {
                name: "priority".to_string(),
                value: "10".to_string(),
            }],
        };
        assert_eq!(endpoint_priority(&ep).unwrap(), Some(10));
    }

    #[test]
    fn endpoint_priority_absent() {
        let ep = Endpoint {
            dns_name: "example.com".to_string(),
            targets: vec!["1.2.3.4".to_string()],
            record_type: "A".to_string(),
            set_identifier: None,
            record_ttl: None,
            labels: Default::default(),
            provider_specific: vec![],
        };
        assert_eq!(endpoint_priority(&ep).unwrap(), None);
    }

    #[test]
    fn endpoint_priority_invalid_returns_error() {
        let ep = Endpoint {
            dns_name: "example.com".to_string(),
            targets: vec!["mail.example.com".to_string()],
            record_type: "MX".to_string(),
            set_identifier: None,
            record_ttl: None,
            labels: Default::default(),
            provider_specific: vec![ProviderSpecific {
                name: "priority".to_string(),
                value: "not-a-number".to_string(),
            }],
        };
        assert!(endpoint_priority(&ep).is_err());
    }

    #[test]
    fn endpoint_ttl_positive() {
        let ep = Endpoint {
            dns_name: "example.com".to_string(),
            targets: vec!["1.2.3.4".to_string()],
            record_type: "A".to_string(),
            set_identifier: None,
            record_ttl: Some(600),
            labels: Default::default(),
            provider_specific: vec![],
        };
        assert_eq!(endpoint_ttl(&ep), Some(600));
    }

    #[test]
    fn endpoint_ttl_zero_is_none() {
        let ep = Endpoint {
            dns_name: "example.com".to_string(),
            targets: vec!["1.2.3.4".to_string()],
            record_type: "A".to_string(),
            set_identifier: None,
            record_ttl: Some(0),
            labels: Default::default(),
            provider_specific: vec![],
        };
        assert_eq!(endpoint_ttl(&ep), None);
    }

    #[test]
    fn endpoint_ttl_absent_is_none() {
        let ep = Endpoint {
            dns_name: "example.com".to_string(),
            targets: vec!["1.2.3.4".to_string()],
            record_type: "A".to_string(),
            set_identifier: None,
            record_ttl: None,
            labels: Default::default(),
            provider_specific: vec![],
        };
        assert_eq!(endpoint_ttl(&ep), None);
    }

    #[test]
    fn endpoint_ttl_overflow_is_none() {
        let ep = Endpoint {
            dns_name: "example.com".to_string(),
            targets: vec!["1.2.3.4".to_string()],
            record_type: "A".to_string(),
            set_identifier: None,
            record_ttl: Some(i64::MAX),
            labels: Default::default(),
            provider_specific: vec![],
        };
        assert_eq!(endpoint_ttl(&ep), None);
    }

    #[test]
    fn endpoint_ttl_negative_is_none() {
        let ep = Endpoint {
            dns_name: "example.com".to_string(),
            targets: vec!["1.2.3.4".to_string()],
            record_type: "A".to_string(),
            set_identifier: None,
            record_ttl: Some(-1),
            labels: Default::default(),
            provider_specific: vec![],
        };
        assert_eq!(endpoint_ttl(&ep), None);
    }

    // --- Normalize FQDN tests ---

    #[test]
    fn normalize_fqdn_basic() {
        assert_eq!(normalize_fqdn("Example.COM"), "example.com");
        assert_eq!(normalize_fqdn("foo.bar."), "foo.bar");
        assert_eq!(normalize_fqdn("  Foo.BAR.  "), "foo.bar");
    }

    // --- Longest suffix match tests ---

    #[test]
    fn longest_suffix_match_basic() {
        let candidates = vec!["example.com".to_string()];
        assert_eq!(
            longest_suffix_match("www.example.com", &candidates),
            Some("example.com".to_string())
        );
    }

    #[test]
    fn longest_suffix_match_multi_label_tld() {
        let candidates = vec!["co.uk".to_string(), "example.co.uk".to_string()];
        assert_eq!(
            longest_suffix_match("app.example.co.uk", &candidates),
            Some("example.co.uk".to_string())
        );
    }

    #[test]
    fn longest_suffix_match_exact() {
        let candidates = vec!["example.com".to_string()];
        assert_eq!(
            longest_suffix_match("example.com", &candidates),
            Some("example.com".to_string())
        );
    }

    #[test]
    fn longest_suffix_match_no_match() {
        let candidates = vec!["example.com".to_string()];
        assert_eq!(longest_suffix_match("other.org", &candidates), None);
    }

    #[test]
    fn longest_suffix_match_rejects_partial() {
        let candidates = vec!["example.com".to_string()];
        // "badexample.com" should NOT match "example.com"
        assert_eq!(longest_suffix_match("badexample.com", &candidates), None);
    }

    // --- TTL/priority equality tests ---

    #[test]
    fn ttl_equal_both_some_same() {
        assert!(ttl_effectively_equal(Some(600), Some(600)));
    }

    #[test]
    fn ttl_equal_both_some_different() {
        assert!(!ttl_effectively_equal(Some(600), Some(300)));
    }

    #[test]
    fn ttl_equal_one_none() {
        assert!(ttl_effectively_equal(Some(600), None));
        assert!(ttl_effectively_equal(None, Some(600)));
    }

    // --- Target normalization tests ---

    #[test]
    fn targets_match_cname_trailing_dot() {
        assert!(targets_match(
            "alias.example.com.",
            "alias.example.com",
            "CNAME"
        ));
    }

    #[test]
    fn targets_match_cname_case_insensitive() {
        assert!(targets_match(
            "Alias.Example.COM",
            "alias.example.com",
            "CNAME"
        ));
    }

    #[test]
    fn targets_match_mx_normalized() {
        assert!(targets_match("mail.Example.COM.", "mail.example.com", "MX"));
    }

    #[test]
    fn targets_match_a_record_exact() {
        // A record targets are IPs — no normalization
        assert!(targets_match("1.2.3.4", "1.2.3.4", "A"));
        assert!(!targets_match("1.2.3.4", "1.2.3.5", "A"));
    }

    #[test]
    fn targets_match_txt_preserves_content() {
        // TXT targets are opaque — no normalization
        assert!(targets_match(
            "v=spf1 include:example.com ~all",
            "v=spf1 include:example.com ~all",
            "TXT"
        ));
        assert!(!targets_match(
            "v=spf1 include:Example.COM ~all",
            "v=spf1 include:example.com ~all",
            "TXT"
        ));
    }

    // --- apply_changes tests ---

    #[tokio::test]
    async fn apply_changes_returns_no_content_on_empty_changes() {
        let handler = test_handler();
        let request: ApplyChangesRequest = serde_json::from_value(json!({
            "create": [],
            "updateOld": [],
            "updateNew": [],
            "delete": []
        }))
        .expect("payload should deserialize");

        let status = handler
            .apply_changes(Ok(Json(request)))
            .await
            .expect("empty changes should succeed");
        assert_eq!(status, StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn apply_changes_rejects_unsupported_record_type() {
        let handler = test_handler();
        let request: ApplyChangesRequest = serde_json::from_value(json!({
            "create": [{
                "dnsName": "app.example.com",
                "targets": ["1.2.3.4"],
                "recordType": "NS"
            }]
        }))
        .expect("payload should deserialize");

        let err = handler
            .apply_changes(Ok(Json(request)))
            .await
            .expect_err("unsupported type should fail");
        assert!(matches!(err, Error::InvalidRequest(_)));
    }

    #[tokio::test]
    async fn apply_changes_rejects_disallowed_domain() {
        let handler = test_handler();
        let request: ApplyChangesRequest = serde_json::from_value(json!({
            "create": [{
                "dnsName": "app.evil.com",
                "targets": ["1.2.3.4"],
                "recordType": "A"
            }]
        }))
        .expect("payload should deserialize");

        let err = handler
            .apply_changes(Ok(Json(request)))
            .await
            .expect_err("disallowed domain should fail");
        assert!(matches!(err, Error::DomainNotAllowed(_)));
    }

    #[tokio::test]
    async fn apply_changes_rejects_blank_target() {
        let handler = test_handler();
        let request: ApplyChangesRequest = serde_json::from_value(json!({
            "create": [{
                "dnsName": "app.example.com",
                "targets": ["1.2.3.4", ""],
                "recordType": "A"
            }]
        }))
        .expect("payload should deserialize");

        let err = handler
            .apply_changes(Ok(Json(request)))
            .await
            .expect_err("blank target should fail");
        assert!(matches!(err, Error::InvalidRequest(_)));
    }

    #[tokio::test]
    async fn apply_changes_rejects_whitespace_target() {
        let handler = test_handler();
        let request: ApplyChangesRequest = serde_json::from_value(json!({
            "create": [{
                "dnsName": "app.example.com",
                "targets": ["  "],
                "recordType": "A"
            }]
        }))
        .expect("payload should deserialize");

        let err = handler
            .apply_changes(Ok(Json(request)))
            .await
            .expect_err("whitespace-only target should fail");
        assert!(matches!(err, Error::InvalidRequest(_)));
    }

    #[tokio::test]
    async fn apply_changes_validates_before_mutation() {
        // Validation should fail fast with mixed valid/invalid endpoints
        let handler = test_handler();
        let request: ApplyChangesRequest = serde_json::from_value(json!({
            "create": [
                {
                    "dnsName": "app.example.com",
                    "targets": ["1.2.3.4"],
                    "recordType": "A"
                },
                {
                    "dnsName": "app.evil.com",
                    "targets": ["1.2.3.4"],
                    "recordType": "A"
                }
            ]
        }))
        .expect("payload should deserialize");

        let err = handler
            .apply_changes(Ok(Json(request)))
            .await
            .expect_err("should fail validation for disallowed domain");
        assert!(matches!(err, Error::DomainNotAllowed(_)));
    }

    #[tokio::test]
    async fn apply_changes_rejects_mismatched_update_lengths() {
        let handler = test_handler();
        let request: ApplyChangesRequest = serde_json::from_value(json!({
            "updateOld": [{
                "dnsName": "app.example.com",
                "targets": ["1.2.3.4"],
                "recordType": "A"
            }],
            "updateNew": [
                {
                    "dnsName": "app.example.com",
                    "targets": ["5.6.7.8"],
                    "recordType": "A"
                },
                {
                    "dnsName": "other.example.com",
                    "targets": ["9.10.11.12"],
                    "recordType": "A"
                }
            ]
        }))
        .expect("payload should deserialize");

        let err = handler
            .apply_changes(Ok(Json(request)))
            .await
            .expect_err("mismatched update lengths should fail");
        assert!(matches!(err, Error::InvalidRequest(_)));
    }
}
