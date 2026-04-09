use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// External-DNS webhook types based on the specification

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Endpoint {
    #[serde(rename = "dnsName")]
    pub dns_name: String,
    pub targets: Vec<String>,
    #[serde(rename = "recordType")]
    pub record_type: String,
    #[serde(rename = "setIdentifier", skip_serializing_if = "Option::is_none")]
    pub set_identifier: Option<String>,
    #[serde(rename = "recordTTL", skip_serializing_if = "Option::is_none")]
    pub record_ttl: Option<i64>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub labels: HashMap<String, String>,
    #[serde(
        rename = "providerSpecific",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    pub provider_specific: Vec<ProviderSpecific>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderSpecific {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Changes {
    #[serde(default, alias = "Create", deserialize_with = "null_as_empty_vec")]
    pub create: Vec<Endpoint>,
    #[serde(default, alias = "UpdateOld", deserialize_with = "null_as_empty_vec")]
    pub update_old: Vec<Endpoint>,
    #[serde(default, alias = "UpdateNew", deserialize_with = "null_as_empty_vec")]
    pub update_new: Vec<Endpoint>,
    #[serde(default, alias = "Delete", deserialize_with = "null_as_empty_vec")]
    pub delete: Vec<Endpoint>,
}

/// Deserialize a `Vec<T>` that treats JSON `null` as an empty vec.
/// This is needed because Go clients may serialize nil slices as `null`.
fn null_as_empty_vec<'de, D, T>(deserializer: D) -> std::result::Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<Vec<T>>::deserialize(deserializer).map(|opt| opt.unwrap_or_default())
}

// Request/Response types for webhook API

#[derive(Debug, Deserialize)]
pub struct GetRecordsQuery {
    #[serde(rename = "zone")]
    pub zone_name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum ApplyChangesRequest {
    // Prefer wrapped requests first so Serde does not eagerly accept them as an empty Direct request.
    Wrapped {
        #[serde(rename = "changes", alias = "Changes")]
        changes: Changes,
    },
    // External-DNS sends changes directly at the root level.
    Direct(Changes),
}

impl ApplyChangesRequest {
    pub fn into_changes(self) -> Changes {
        match self {
            ApplyChangesRequest::Direct(changes) => changes,
            ApplyChangesRequest::Wrapped { changes } => changes,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
}

// Helper implementations

impl Endpoint {
    pub fn from_porkbun_record(record: &crate::porkbun::DnsRecord, zone: &str) -> Self {
        // Porkbun returns the FQDN in `name`.
        // For apex, the name equals the zone.
        // Normalize both sides to lowercase for case-insensitive comparison.
        let name_lower = record.name.to_ascii_lowercase();
        let zone_lower = zone.to_ascii_lowercase();
        let dns_name = if name_lower.is_empty() || name_lower == zone_lower {
            zone_lower.clone()
        } else if name_lower.ends_with(&format!(".{zone_lower}")) {
            name_lower.clone()
        } else {
            format!("{name_lower}.{zone_lower}")
        };

        Self {
            dns_name,
            targets: vec![record.content.clone()],
            record_type: record.record_type.clone(),
            set_identifier: None,
            record_ttl: record.ttl_u32().map(|ttl| ttl as i64),
            labels: HashMap::new(),
            provider_specific: match record.prio_u32() {
                Some(prio) => vec![ProviderSpecific {
                    name: "priority".to_string(),
                    value: prio.to_string(),
                }],
                None => Vec::new(),
            },
        }
    }
}

/// Groups multiple Porkbun DNS records into ExternalDNS endpoints.
/// Records with the same (dns_name, record_type, ttl, priority) are merged
/// into a single Endpoint with multiple targets.
/// Results are sorted by (dns_name, record_type), targets are sorted.
pub fn group_porkbun_records(records: &[crate::porkbun::DnsRecord], zone: &str) -> Vec<Endpoint> {
    // Key: (dns_name, record_type, ttl, priority)
    let mut groups: HashMap<(String, String, Option<i64>, Option<u32>), Endpoint> = HashMap::new();

    for record in records {
        let endpoint = Endpoint::from_porkbun_record(record, zone);
        let prio = record.prio_u32();
        let key = (
            endpoint.dns_name.clone(),
            endpoint.record_type.clone(),
            endpoint.record_ttl,
            prio,
        );

        groups
            .entry(key)
            .and_modify(|existing| {
                for target in &endpoint.targets {
                    if !existing.targets.contains(target) {
                        existing.targets.push(target.clone());
                    }
                }
            })
            .or_insert(endpoint);
    }

    let mut endpoints: Vec<Endpoint> = groups.into_values().collect();

    // Sort endpoints deterministically
    endpoints.sort_by(|a, b| {
        a.dns_name
            .cmp(&b.dns_name)
            .then(a.record_type.cmp(&b.record_type))
    });

    // Sort targets within each endpoint
    for ep in &mut endpoints {
        ep.targets.sort();
    }

    endpoints
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.create.is_empty()
            && self.update_old.is_empty()
            && self.update_new.is_empty()
            && self.delete.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::porkbun::DnsRecord;
    use serde_json::json;

    fn sample_endpoint(name: &str) -> serde_json::Value {
        json!({
            "dnsName": name,
            "targets": ["192.0.2.10"],
            "recordType": "A"
        })
    }

    #[test]
    fn deserializes_external_dns_lower_camel_case_changes() {
        let request: ApplyChangesRequest = serde_json::from_value(json!({
            "create": [sample_endpoint("new.example.com")],
            "updateOld": [sample_endpoint("old.example.com")],
            "updateNew": [sample_endpoint("newer.example.com")],
            "delete": [sample_endpoint("delete.example.com")]
        }))
        .expect("lowerCamelCase payload should deserialize");

        let changes = request.into_changes();

        assert_eq!(changes.create.len(), 1);
        assert_eq!(changes.update_old.len(), 1);
        assert_eq!(changes.update_new.len(), 1);
        assert_eq!(changes.delete.len(), 1);
        assert_eq!(changes.create[0].dns_name, "new.example.com");
    }

    #[test]
    fn deserializes_legacy_pascal_case_changes() {
        let request: ApplyChangesRequest = serde_json::from_value(json!({
            "Create": [sample_endpoint("new.example.com")],
            "UpdateOld": [sample_endpoint("old.example.com")],
            "UpdateNew": [sample_endpoint("newer.example.com")],
            "Delete": [sample_endpoint("delete.example.com")]
        }))
        .expect("PascalCase payload should deserialize");

        let changes = request.into_changes();

        assert_eq!(changes.create.len(), 1);
        assert_eq!(changes.update_old.len(), 1);
        assert_eq!(changes.update_new.len(), 1);
        assert_eq!(changes.delete.len(), 1);
    }

    #[test]
    fn deserializes_null_arrays_as_empty() {
        // Go clients may serialize nil slices as JSON null
        let request: ApplyChangesRequest = serde_json::from_value(json!({
            "create": null,
            "updateOld": null,
            "updateNew": null,
            "delete": null
        }))
        .expect("null arrays should deserialize as empty");

        let changes = request.into_changes();
        assert!(changes.create.is_empty());
        assert!(changes.update_old.is_empty());
        assert!(changes.update_new.is_empty());
        assert!(changes.delete.is_empty());
        assert!(changes.is_empty());
    }

    #[test]
    fn deserializes_mixed_null_and_present_arrays() {
        let request: ApplyChangesRequest = serde_json::from_value(json!({
            "create": [sample_endpoint("new.example.com")],
            "delete": null
        }))
        .expect("mixed null/present arrays should deserialize");

        let changes = request.into_changes();
        assert_eq!(changes.create.len(), 1);
        assert!(changes.delete.is_empty());
    }

    #[test]
    fn deserializes_wrapped_changes_payload() {
        let request: ApplyChangesRequest = serde_json::from_value(json!({
            "changes": {
                "create": [sample_endpoint("wrapped.example.com")]
            }
        }))
        .expect("wrapped payload should deserialize");

        let changes = request.into_changes();
        assert_eq!(changes.create.len(), 1);
        assert_eq!(changes.create[0].dns_name, "wrapped.example.com");
    }

    #[test]
    fn from_porkbun_record_apex() {
        let record = DnsRecord {
            id: "1".to_string(),
            name: "example.com".to_string(),
            record_type: "A".to_string(),
            content: "1.2.3.4".to_string(),
            ttl: Some("600".to_string()),
            prio: None,
        };
        let ep = Endpoint::from_porkbun_record(&record, "example.com");
        assert_eq!(ep.dns_name, "example.com");
        assert_eq!(ep.targets, vec!["1.2.3.4"]);
        assert_eq!(ep.record_ttl, Some(600));
        assert!(ep.provider_specific.is_empty());
    }

    #[test]
    fn from_porkbun_record_subdomain() {
        let record = DnsRecord {
            id: "2".to_string(),
            name: "www.example.com".to_string(),
            record_type: "CNAME".to_string(),
            content: "other.example.com".to_string(),
            ttl: Some("300".to_string()),
            prio: None,
        };
        let ep = Endpoint::from_porkbun_record(&record, "example.com");
        assert_eq!(ep.dns_name, "www.example.com");
    }

    #[test]
    fn from_porkbun_record_mx_with_priority() {
        let record = DnsRecord {
            id: "3".to_string(),
            name: "example.com".to_string(),
            record_type: "MX".to_string(),
            content: "mail.example.com".to_string(),
            ttl: Some("3600".to_string()),
            prio: Some("10".to_string()),
        };
        let ep = Endpoint::from_porkbun_record(&record, "example.com");
        assert_eq!(ep.provider_specific.len(), 1);
        assert_eq!(ep.provider_specific[0].name, "priority");
        assert_eq!(ep.provider_specific[0].value, "10");
    }

    #[test]
    fn from_porkbun_record_rejects_partial_domain_match() {
        // "badexample.com" should NOT be treated as belonging to zone "example.com"
        let record = DnsRecord {
            id: "4".to_string(),
            name: "badexample.com".to_string(),
            record_type: "A".to_string(),
            content: "1.2.3.4".to_string(),
            ttl: Some("600".to_string()),
            prio: None,
        };
        let ep = Endpoint::from_porkbun_record(&record, "example.com");
        // Should be treated as a subdomain-style name, not matching the zone
        assert_eq!(ep.dns_name, "badexample.com.example.com");
    }

    #[test]
    fn from_porkbun_record_case_insensitive() {
        let record = DnsRecord {
            id: "6".to_string(),
            name: "WWW.Example.COM".to_string(),
            record_type: "A".to_string(),
            content: "1.2.3.4".to_string(),
            ttl: Some("600".to_string()),
            prio: None,
        };
        let ep = Endpoint::from_porkbun_record(&record, "example.com");
        assert_eq!(ep.dns_name, "www.example.com");
    }

    #[test]
    fn from_porkbun_record_apex_case_insensitive() {
        let record = DnsRecord {
            id: "7".to_string(),
            name: "EXAMPLE.COM".to_string(),
            record_type: "A".to_string(),
            content: "1.2.3.4".to_string(),
            ttl: Some("600".to_string()),
            prio: None,
        };
        let ep = Endpoint::from_porkbun_record(&record, "Example.COM");
        assert_eq!(ep.dns_name, "example.com");
    }

    #[test]
    fn from_porkbun_record_mx_priority_zero() {
        let record = DnsRecord {
            id: "5".to_string(),
            name: "example.com".to_string(),
            record_type: "MX".to_string(),
            content: "mail.example.com".to_string(),
            ttl: Some("3600".to_string()),
            prio: Some("0".to_string()),
        };
        let ep = Endpoint::from_porkbun_record(&record, "example.com");
        assert_eq!(ep.provider_specific.len(), 1);
        assert_eq!(ep.provider_specific[0].name, "priority");
        assert_eq!(ep.provider_specific[0].value, "0");
    }

    #[test]
    fn group_porkbun_records_merges_targets() {
        let records = vec![
            DnsRecord {
                id: "1".to_string(),
                name: "www.example.com".to_string(),
                record_type: "A".to_string(),
                content: "1.2.3.4".to_string(),
                ttl: Some("600".to_string()),
                prio: None,
            },
            DnsRecord {
                id: "2".to_string(),
                name: "www.example.com".to_string(),
                record_type: "A".to_string(),
                content: "5.6.7.8".to_string(),
                ttl: Some("600".to_string()),
                prio: None,
            },
        ];

        let endpoints = group_porkbun_records(&records, "example.com");
        assert_eq!(endpoints.len(), 1);
        assert_eq!(endpoints[0].targets, vec!["1.2.3.4", "5.6.7.8"]);
    }

    #[test]
    fn group_porkbun_records_different_types_not_merged() {
        let records = vec![
            DnsRecord {
                id: "1".to_string(),
                name: "example.com".to_string(),
                record_type: "A".to_string(),
                content: "1.2.3.4".to_string(),
                ttl: Some("600".to_string()),
                prio: None,
            },
            DnsRecord {
                id: "2".to_string(),
                name: "example.com".to_string(),
                record_type: "AAAA".to_string(),
                content: "::1".to_string(),
                ttl: Some("600".to_string()),
                prio: None,
            },
        ];

        let endpoints = group_porkbun_records(&records, "example.com");
        assert_eq!(endpoints.len(), 2);
    }

    #[test]
    fn group_porkbun_records_sorted_deterministically() {
        let records = vec![
            DnsRecord {
                id: "1".to_string(),
                name: "zzz.example.com".to_string(),
                record_type: "A".to_string(),
                content: "1.1.1.1".to_string(),
                ttl: Some("600".to_string()),
                prio: None,
            },
            DnsRecord {
                id: "2".to_string(),
                name: "aaa.example.com".to_string(),
                record_type: "A".to_string(),
                content: "2.2.2.2".to_string(),
                ttl: Some("600".to_string()),
                prio: None,
            },
        ];

        let endpoints = group_porkbun_records(&records, "example.com");
        assert_eq!(endpoints[0].dns_name, "aaa.example.com");
        assert_eq!(endpoints[1].dns_name, "zzz.example.com");
    }
}
