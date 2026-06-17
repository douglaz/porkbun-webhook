use super::types::*;
use crate::error::{Error, Result};
use reqwest::Client as HttpClient;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::time::Duration;
use tracing::{debug, info, warn};

pub struct Client {
    http_client: HttpClient,
    api_key: String,
    secret_api_key: String,
    base_url: String,
}

impl Client {
    pub fn new(
        api_key: &str,
        secret_api_key: &str,
        base_url: &str,
        timeout: Duration,
    ) -> Result<Self> {
        let http_client = HttpClient::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| Error::Configuration(format!("Failed to create HTTP client: {e}")))?;

        Ok(Self {
            http_client,
            api_key: api_key.to_string(),
            secret_api_key: secret_api_key.to_string(),
            base_url: base_url.trim_end_matches('/').to_string(),
        })
    }

    /// Internal helper: POST JSON to a Porkbun endpoint and deserialize the response.
    async fn post_json<TReq, TResp>(&self, path: &str, body: &TReq) -> Result<TResp>
    where
        TReq: Serialize + ?Sized,
        TResp: DeserializeOwned,
    {
        let url = format!("{}{}", self.base_url, path);
        debug!(path, "Porkbun API request");

        let response = self
            .http_client
            .post(&url)
            .json(body)
            .send()
            .await
            .map_err(Error::Network)?;

        let status = response.status();
        debug!(path, %status, "Porkbun API response");

        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(Error::RateLimited);
        }

        if status == reqwest::StatusCode::UNAUTHORIZED {
            let text = response.text().await.unwrap_or_default();
            return Err(Error::Authentication(format!("HTTP {status}: {text}")));
        }

        if status == reqwest::StatusCode::FORBIDDEN {
            let text = response.text().await.unwrap_or_default();
            // Porkbun may return 403 for domain-level API access issues
            // (e.g. API access not enabled for a domain), not just bad credentials.
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text)
                && let Some(msg) = parsed.get("message").and_then(|m| m.as_str())
            {
                let lower = msg.to_lowercase();
                if lower.contains("domain")
                    || lower.contains("api access")
                    || lower.contains("not enabled")
                {
                    return Err(Error::DomainNotAllowed(format!("HTTP {status}: {msg}")));
                }
            }
            return Err(Error::Authentication(format!("HTTP {status}: {text}")));
        }

        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();

            // 5xx from Porkbun indicates an upstream outage — transient
            if status.is_server_error() {
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text)
                    && let Some(msg) = parsed.get("message").and_then(|m| m.as_str())
                {
                    return Err(Error::PorkbunUpstream(format!("HTTP {status}: {msg}")));
                }
                return Err(Error::PorkbunUpstream(format!("HTTP {status}: {text}")));
            }

            // 4xx (non-429/401/403) — inspect for auth or not-found errors
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text)
                && let Some(msg) = parsed.get("message").and_then(|m| m.as_str())
            {
                let lower = msg.to_lowercase();
                // Porkbun sends 400 for bad API keys with messages like
                // "Invalid API key" or "Invalid secret API key"
                if lower.contains("invalid api key")
                    || lower.contains("invalid secret")
                    || lower.contains("authentication")
                    || lower.contains("apikey")
                {
                    return Err(Error::Authentication(format!("HTTP {status}: {msg}")));
                }
                // Record-not-found errors (e.g. deleting already-deleted record)
                if lower.contains("not found") || lower.contains("does not exist") {
                    return Err(Error::RecordNotFound(format!("HTTP {status}: {msg}")));
                }
                return Err(Error::PorkbunApi(format!("HTTP {status}: {msg}")));
            }
            return Err(Error::PorkbunApi(format!("HTTP {status}: {text}")));
        }

        let resp: TResp = response.json().await?;
        Ok(resp)
    }

    /// Classify a Porkbun API error from an in-band `{"status":"ERROR","message":"..."}` response.
    /// Inspects the message to distinguish auth failures from generic API errors.
    fn classify_api_error(context: &str, message: &str) -> Error {
        let lower = message.to_lowercase();
        if lower.contains("invalid api key")
            || lower.contains("invalid secret")
            || lower.contains("authentication")
            || lower.contains("apikey")
        {
            Error::Authentication(format!("{context}: {message}"))
        } else if lower.contains("not found") || lower.contains("does not exist") {
            Error::RecordNotFound(format!("{context}: {message}"))
        } else {
            Error::PorkbunApi(format!("{context}: {message}"))
        }
    }

    /// Returns an auth body to embed in requests.
    fn auth_body(&self) -> AuthBody {
        AuthBody {
            apikey: self.api_key.clone(),
            secretapikey: self.secret_api_key.clone(),
        }
    }

    /// Ping the Porkbun API to validate credentials.
    pub async fn ping(&self) -> Result<PingResponse> {
        let body = self.auth_body();
        let resp: PingResponse = self.post_json("/ping", &body).await?;
        if resp.status != "SUCCESS" {
            let msg = resp.message.as_deref().unwrap_or(&resp.status);
            return Err(Self::classify_api_error("ping", msg));
        }
        Ok(resp)
    }

    /// List all domains in the account, paginating through all results.
    /// Porkbun returns up to 1000 domains per request.
    pub async fn list_domains(&self) -> Result<Vec<Domain>> {
        const PAGE_SIZE: usize = 1000;
        let mut all_domains = Vec::new();
        let mut start: u32 = 0;

        loop {
            let body = ListAllRequest {
                apikey: self.api_key.clone(),
                secretapikey: self.secret_api_key.clone(),
                start: if start == 0 { None } else { Some(start) },
            };
            let resp: ListAllResponse = self.post_json("/domain/listAll", &body).await?;
            if resp.status != "SUCCESS" {
                let msg = resp.message.as_deref().unwrap_or(&resp.status);
                return Err(Self::classify_api_error("list_domains", msg));
            }
            let page = resp.domains.unwrap_or_default();
            let page_len = page.len();
            all_domains.extend(page);

            if page_len < PAGE_SIZE {
                break;
            }
            start += page_len as u32;
        }

        info!("Listed {} domains (paginated)", all_domains.len());
        Ok(all_domains)
    }

    /// List all DNS records for a domain.
    pub async fn list_records(&self, domain: &str) -> Result<Vec<DnsRecord>> {
        let body = self.auth_body();
        let path = format!("/dns/retrieve/{domain}");
        let resp: DnsRetrieveResponse = self.post_json(&path, &body).await?;
        if resp.status != "SUCCESS" {
            let msg = resp.message.as_deref().unwrap_or(&resp.status);
            return Err(Self::classify_api_error(
                &format!("list_records({domain})"),
                msg,
            ));
        }
        let records = resp.records.unwrap_or_default();
        info!("Listed {} records for domain {}", records.len(), domain);
        Ok(records)
    }

    /// Create a DNS record. Returns the new record ID.
    pub async fn add_record(&self, domain: &str, params: CreateDnsParams) -> Result<String> {
        let body = CreateDnsRequest {
            apikey: self.api_key.clone(),
            secretapikey: self.secret_api_key.clone(),
            name: if params.subdomain.is_empty() {
                None
            } else {
                Some(params.subdomain)
            },
            record_type: params.record_type,
            content: params.content,
            ttl: params.ttl,
            prio: params.prio,
        };

        let path = format!("/dns/create/{domain}");
        let resp: CreateDnsResponse = self.post_json(&path, &body).await?;
        if resp.status != "SUCCESS" {
            let msg = resp.message.as_deref().unwrap_or(&resp.status);
            return Err(Self::classify_api_error(
                &format!("add_record({domain})"),
                msg,
            ));
        }

        let id = resp.id_string().unwrap_or_default();
        info!("Created record {id} for domain {domain}");
        Ok(id)
    }

    /// Edit a DNS record by ID.
    pub async fn edit_record(&self, domain: &str, id: &str, params: EditDnsParams) -> Result<()> {
        let body = EditDnsRequest {
            apikey: self.api_key.clone(),
            secretapikey: self.secret_api_key.clone(),
            name: if params.subdomain.is_empty() {
                None
            } else {
                Some(params.subdomain)
            },
            record_type: params.record_type,
            content: params.content,
            ttl: params.ttl,
            prio: params.prio,
        };

        let path = format!("/dns/edit/{domain}/{id}");
        let resp: BasicResponse = self.post_json(&path, &body).await?;
        if resp.status != "SUCCESS" {
            let msg = resp.message.as_deref().unwrap_or(&resp.status);
            return Err(Self::classify_api_error(
                &format!("edit_record({domain}/{id})"),
                msg,
            ));
        }

        info!("Edited record {id} for domain {domain}");
        Ok(())
    }

    /// Delete a DNS record by ID. Idempotent: already-deleted records are not errors.
    pub async fn remove_record(&self, domain: &str, id: &str) -> Result<()> {
        let body = self.auth_body();
        let path = format!("/dns/delete/{domain}/{id}");
        let resp = self.post_json::<_, BasicResponse>(&path, &body).await;

        match resp {
            Ok(r) if r.status == "SUCCESS" => {
                info!("Removed record {id} from domain {domain}");
                Ok(())
            }
            Ok(r) => {
                let msg = r.message.unwrap_or_default();
                let lower = msg.to_lowercase();
                if lower.contains("not found") || lower.contains("does not exist") {
                    warn!("Record {id} already deleted from {domain}");
                    Ok(())
                } else {
                    Err(Error::PorkbunApi(format!(
                        "remove_record({domain}/{id}) failed: {msg}"
                    )))
                }
            }
            Err(Error::RecordNotFound(_)) => {
                // post_json detected a not-found response at the HTTP level
                warn!("Record {id} already deleted from {domain}");
                Ok(())
            }
            Err(e) => Err(e),
        }
    }
}
