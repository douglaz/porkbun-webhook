use serde::{Deserialize, Serialize};

// --- Authentication ---

#[derive(Debug, Serialize)]
pub struct AuthBody {
    pub apikey: String,
    pub secretapikey: String,
}

// --- Ping ---

#[derive(Debug, Deserialize)]
pub struct PingResponse {
    pub status: String,
    #[serde(rename = "yourIp")]
    #[allow(dead_code)]
    pub your_ip: Option<String>,
    pub message: Option<String>,
}

// --- Domain listing ---

#[derive(Debug, Serialize)]
pub struct ListAllRequest {
    pub apikey: String,
    pub secretapikey: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct ListAllResponse {
    pub status: String,
    #[allow(dead_code)]
    pub message: Option<String>,
    pub domains: Option<Vec<Domain>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Domain {
    pub domain: String,
    // Porkbun returns additional fields, but we only need the domain name.
}

// --- DNS record retrieval ---

#[derive(Debug, Deserialize)]
pub struct DnsRetrieveResponse {
    pub status: String,
    pub message: Option<String>,
    pub records: Option<Vec<DnsRecord>>,
}

/// A DNS record as returned by Porkbun's retrieve endpoint.
/// Porkbun may serialize numeric-looking fields as strings, so we
/// accept strings and provide typed helpers.
#[derive(Debug, Clone, Deserialize)]
pub struct DnsRecord {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub record_type: String,
    pub content: String,
    pub ttl: Option<String>,
    pub prio: Option<String>,
}

impl DnsRecord {
    pub fn ttl_u32(&self) -> Option<u32> {
        self.ttl.as_ref().and_then(|v| v.parse().ok())
    }

    pub fn prio_u32(&self) -> Option<u32> {
        self.prio.as_ref().and_then(|v| v.parse().ok())
    }
}

// --- DNS record creation ---

#[derive(Debug, Serialize)]
pub struct CreateDnsRequest {
    pub apikey: String,
    pub secretapikey: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(rename = "type")]
    pub record_type: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttl: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prio: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct CreateDnsResponse {
    pub status: String,
    pub message: Option<String>,
    pub id: Option<serde_json::Value>,
}

impl CreateDnsResponse {
    /// Extract the record ID as a string regardless of whether Porkbun
    /// returns it as a number or string.
    pub fn id_string(&self) -> Option<String> {
        self.id.as_ref().map(|v| match v {
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        })
    }
}

// --- DNS record editing ---

#[derive(Debug, Serialize)]
pub struct EditDnsRequest {
    pub apikey: String,
    pub secretapikey: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(rename = "type")]
    pub record_type: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttl: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prio: Option<u32>,
}

// --- Generic response for delete / edit ---

#[derive(Debug, Deserialize)]
pub struct BasicResponse {
    pub status: String,
    pub message: Option<String>,
}

// --- Params passed internally (without credentials) ---

#[derive(Debug)]
pub struct CreateDnsParams {
    pub subdomain: String,
    pub record_type: String,
    pub content: String,
    pub ttl: Option<u32>,
    pub prio: Option<u32>,
}

#[derive(Debug)]
pub struct EditDnsParams {
    pub subdomain: String,
    pub record_type: String,
    pub content: String,
    pub ttl: Option<u32>,
    pub prio: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dns_record_ttl_parsing() {
        let record = DnsRecord {
            id: "1".to_string(),
            name: "example.com".to_string(),
            record_type: "A".to_string(),
            content: "1.2.3.4".to_string(),
            ttl: Some("600".to_string()),
            prio: None,
        };
        assert_eq!(record.ttl_u32(), Some(600));
    }

    #[test]
    fn dns_record_ttl_none() {
        let record = DnsRecord {
            id: "1".to_string(),
            name: "example.com".to_string(),
            record_type: "A".to_string(),
            content: "1.2.3.4".to_string(),
            ttl: None,
            prio: None,
        };
        assert_eq!(record.ttl_u32(), None);
    }

    #[test]
    fn dns_record_ttl_invalid() {
        let record = DnsRecord {
            id: "1".to_string(),
            name: "example.com".to_string(),
            record_type: "A".to_string(),
            content: "1.2.3.4".to_string(),
            ttl: Some("invalid".to_string()),
            prio: None,
        };
        assert_eq!(record.ttl_u32(), None);
    }

    #[test]
    fn dns_record_prio_parsing() {
        let record = DnsRecord {
            id: "1".to_string(),
            name: "example.com".to_string(),
            record_type: "MX".to_string(),
            content: "mail.example.com".to_string(),
            ttl: Some("600".to_string()),
            prio: Some("10".to_string()),
        };
        assert_eq!(record.prio_u32(), Some(10));
    }

    #[test]
    fn dns_record_prio_zero_is_some() {
        let record = DnsRecord {
            id: "1".to_string(),
            name: "example.com".to_string(),
            record_type: "A".to_string(),
            content: "1.2.3.4".to_string(),
            ttl: Some("600".to_string()),
            prio: Some("0".to_string()),
        };
        assert_eq!(record.prio_u32(), Some(0));
    }

    #[test]
    fn create_dns_response_id_as_number() {
        let resp: CreateDnsResponse =
            serde_json::from_str(r#"{"status":"SUCCESS","id":12345}"#).unwrap();
        assert_eq!(resp.id_string(), Some("12345".to_string()));
    }

    #[test]
    fn create_dns_response_id_as_string() {
        let resp: CreateDnsResponse =
            serde_json::from_str(r#"{"status":"SUCCESS","id":"12345"}"#).unwrap();
        assert_eq!(resp.id_string(), Some("12345".to_string()));
    }
}
