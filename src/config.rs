use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::env;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    pub porkbun_api_key: String,
    pub porkbun_secret_api_key: String,
    pub porkbun_api_base: String,
    pub webhook_host: String,
    pub webhook_port: u16,
    pub domain_filter: Option<Vec<String>>,
    pub dry_run: bool,
    pub cache_ttl_seconds: u64,
    pub http_timeout_seconds: u64,
    pub trace_request_bodies: bool,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        dotenvy::dotenv().ok();

        let porkbun_api_key = env::var("PORKBUN_API_KEY")
            .map_err(|_| anyhow::anyhow!("PORKBUN_API_KEY environment variable is required"))?;

        let porkbun_secret_api_key = env::var("PORKBUN_SECRET_API_KEY").map_err(|_| {
            anyhow::anyhow!("PORKBUN_SECRET_API_KEY environment variable is required")
        })?;

        let porkbun_api_base = env::var("PORKBUN_API_BASE")
            .unwrap_or_else(|_| "https://api.porkbun.com/api/json/v3".to_string());

        let webhook_host = env::var("WEBHOOK_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());

        let webhook_port = env::var("WEBHOOK_PORT")
            .unwrap_or_else(|_| "8888".to_string())
            .parse::<u16>()?;

        let domain_filter = env::var("DOMAIN_FILTER").ok().and_then(|s| {
            let domains: Vec<String> = s
                .split(',')
                .map(|d| Self::normalize_domain(d.trim()))
                .filter(|d| !d.is_empty())
                .collect();
            if domains.is_empty() {
                None
            } else {
                Some(domains)
            }
        });

        let dry_run = env::var("DRY_RUN")
            .unwrap_or_else(|_| "false".to_string())
            .parse::<bool>()?;

        let cache_ttl_seconds = env::var("CACHE_TTL_SECONDS")
            .unwrap_or_else(|_| "60".to_string())
            .parse::<u64>()?;

        let http_timeout_seconds = env::var("HTTP_TIMEOUT_SECONDS")
            .unwrap_or_else(|_| "30".to_string())
            .parse::<u64>()?;

        let trace_request_bodies = env::var("TRACE_REQUEST_BODIES")
            .unwrap_or_else(|_| "false".to_string())
            .parse::<bool>()?;

        Ok(Config {
            porkbun_api_key,
            porkbun_secret_api_key,
            porkbun_api_base,
            webhook_host,
            webhook_port,
            domain_filter,
            dry_run,
            cache_ttl_seconds,
            http_timeout_seconds,
            trace_request_bodies,
        })
    }

    pub fn is_domain_allowed(&self, domain: &str) -> bool {
        match &self.domain_filter {
            Some(filter) => {
                let domain = Self::normalize_domain(domain);
                filter
                    .iter()
                    .any(|d| domain == *d || domain.ends_with(&format!(".{d}")))
            }
            None => true,
        }
    }

    /// Returns the normalized domain filter entries, or an empty vec if unset.
    pub fn normalized_domain_filter(&self) -> Vec<String> {
        self.domain_filter.clone().unwrap_or_default()
    }

    /// Canonicalize a domain name: trim, strip trailing dot, and lowercase.
    pub fn normalize_domain(s: &str) -> String {
        s.trim()
            .strip_suffix('.')
            .unwrap_or(s.trim())
            .to_ascii_lowercase()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with_filter(domains: Vec<&str>) -> Config {
        Config {
            porkbun_api_key: String::new(),
            porkbun_secret_api_key: String::new(),
            porkbun_api_base: String::new(),
            webhook_host: String::new(),
            webhook_port: 8888,
            domain_filter: Some(domains.into_iter().map(Config::normalize_domain).collect()),
            dry_run: false,
            cache_ttl_seconds: 60,
            http_timeout_seconds: 30,
            trace_request_bodies: false,
        }
    }

    #[test]
    fn exact_match_is_allowed() {
        let config = config_with_filter(vec!["example.com"]);
        assert!(config.is_domain_allowed("example.com"));
    }

    #[test]
    fn proper_subdomain_is_allowed() {
        let config = config_with_filter(vec!["example.com"]);
        assert!(config.is_domain_allowed("www.example.com"));
    }

    #[test]
    fn nested_subdomain_is_allowed() {
        let config = config_with_filter(vec!["example.com"]);
        assert!(config.is_domain_allowed("sub.deep.example.com"));
    }

    #[test]
    fn sibling_domain_is_rejected() {
        let config = config_with_filter(vec!["example.com"]);
        assert!(!config.is_domain_allowed("badexample.com"));
    }

    #[test]
    fn another_sibling_domain_is_rejected() {
        let config = config_with_filter(vec!["example.com"]);
        assert!(!config.is_domain_allowed("notexample.com"));
    }

    #[test]
    fn case_insensitive_match() {
        let config = config_with_filter(vec!["example.com"]);
        assert!(config.is_domain_allowed("WWW.Example.COM"));
    }

    #[test]
    fn trailing_dot_on_domain() {
        let config = config_with_filter(vec!["example.com"]);
        assert!(config.is_domain_allowed("www.example.com."));
    }

    #[test]
    fn trailing_dot_on_filter() {
        let config = config_with_filter(vec!["example.com."]);
        assert!(config.is_domain_allowed("www.example.com"));
    }

    #[test]
    fn none_filter_allows_all() {
        let config = Config {
            porkbun_api_key: String::new(),
            porkbun_secret_api_key: String::new(),
            porkbun_api_base: String::new(),
            webhook_host: String::new(),
            webhook_port: 8888,
            domain_filter: None,
            dry_run: false,
            cache_ttl_seconds: 60,
            http_timeout_seconds: 30,
            trace_request_bodies: false,
        };
        assert!(config.is_domain_allowed("anything.com"));
    }

    #[test]
    fn normalized_domain_filter_returns_empty_when_unset() {
        let config = Config {
            porkbun_api_key: String::new(),
            porkbun_secret_api_key: String::new(),
            porkbun_api_base: String::new(),
            webhook_host: String::new(),
            webhook_port: 8888,
            domain_filter: None,
            dry_run: false,
            cache_ttl_seconds: 60,
            http_timeout_seconds: 30,
            trace_request_bodies: false,
        };
        assert!(config.normalized_domain_filter().is_empty());
    }

    #[test]
    fn empty_filter_allows_all() {
        // Simulates DOMAIN_FILTER="" — should be treated as None (allow all)
        let config = Config {
            porkbun_api_key: String::new(),
            porkbun_secret_api_key: String::new(),
            porkbun_api_base: String::new(),
            webhook_host: String::new(),
            webhook_port: 8888,
            domain_filter: None, // empty string becomes None after from_env fix
            dry_run: false,
            cache_ttl_seconds: 60,
            http_timeout_seconds: 30,
            trace_request_bodies: false,
        };
        assert!(config.is_domain_allowed("anything.com"));
        assert!(config.normalized_domain_filter().is_empty());
    }

    #[test]
    fn normalize_domain_trims_and_lowercases() {
        assert_eq!(Config::normalize_domain("  Example.COM. "), "example.com");
        assert_eq!(Config::normalize_domain("foo.bar"), "foo.bar");
        assert_eq!(Config::normalize_domain(""), "");
    }
}
