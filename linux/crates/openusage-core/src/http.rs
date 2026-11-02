use std::collections::BTreeMap;
use std::io::Read;
use std::time::{Duration, Instant};

use crate::error::ProviderError;
use crate::log;

#[derive(Debug, Clone, PartialEq)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub body: Option<Vec<u8>>,
    pub timeout: Duration,
}

impl HttpRequest {
    pub fn new(method: &str, url: impl Into<String>) -> Self {
        Self {
            method: method.into(),
            url: url.into(),
            headers: BTreeMap::new(),
            body: None,
            timeout: Duration::from_secs(15),
        }
    }

    pub fn get(url: impl Into<String>) -> Self {
        Self::new("GET", url)
    }

    pub fn post(url: impl Into<String>) -> Self {
        Self::new("POST", url)
    }

    pub fn header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.insert(name.to_string(), value.into());
        self
    }

    pub fn bearer(self, token: &str) -> Self {
        self.header("Authorization", format!("Bearer {token}"))
    }

    pub fn json_body(mut self, value: &serde_json::Value) -> Self {
        self.body = Some(serde_json::to_vec(value).unwrap_or_default());
        self.header("Content-Type", "application/json")
    }

    /// An `application/x-www-form-urlencoded` body, fields sorted for stable requests.
    pub fn form_body(mut self, fields: &[(&str, &str)]) -> Self {
        let mut pairs: Vec<String> = fields
            .iter()
            .map(|(key, value)| format!("{}={}", form_encode(key), form_encode(value)))
            .collect();
        pairs.sort();
        self.body = Some(pairs.join("&").into_bytes());
        self.header("Content-Type", "application/x-www-form-urlencoded")
    }
}

pub fn form_encode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct HttpResponse {
    pub status: u16,
    /// Lower-cased header names.
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }

    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn json(&self) -> Option<serde_json::Value> {
        serde_json::from_slice(&self.body).ok()
    }

    pub fn decode<T: serde::de::DeserializeOwned>(&self) -> Result<T, ProviderError> {
        serde_json::from_slice(&self.body).map_err(|_| ProviderError::InvalidResponse)
    }

    /// `Retry-After` in seconds (a number or an HTTP date), capped at the failure backoff cap.
    pub fn retry_after_seconds(&self) -> Option<f64> {
        let value = self.header("retry-after")?.trim();
        let seconds = value.parse::<f64>().ok().or_else(|| {
            chrono::DateTime::parse_from_rfc2822(value)
                .ok()
                .map(|date| {
                    (date.with_timezone(&chrono::Utc) - chrono::Utc::now()).num_milliseconds()
                        as f64
                        / 1000.0
                })
        })?;
        (seconds.is_finite() && seconds > 0.0)
            .then(|| seconds.min(crate::refresh::FAILURE_BACKOFF_CAP_SECONDS as f64))
    }
}

/// Every provider request goes through this, so providers can be tested with a fake.
pub trait HttpClient: Send + Sync {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, ProviderError>;
}

/// The real client: ureq with rustls, the proxy from `~/.openusage/config.json`, no cache.
pub struct UreqClient {
    agent: ureq::Agent,
}

impl UreqClient {
    pub fn new() -> Self {
        let mut builder =
            ureq::AgentBuilder::new().user_agent(&format!("OpenUsage/{}", crate::VERSION));
        if let Some(proxy) = crate::proxy::load() {
            match ureq::Proxy::new(proxy.ureq_url()) {
                Ok(proxy) => builder = builder.proxy(proxy),
                Err(error) => log::error("config", &format!("proxy off: {error}")),
            }
        }
        Self {
            agent: builder.build(),
        }
    }
}

impl Default for UreqClient {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpClient for UreqClient {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, ProviderError> {
        let start = Instant::now();
        let mut call = self
            .agent
            .request(&request.method, &request.url)
            .timeout(request.timeout);
        for (name, value) in &request.headers {
            call = call.set(name, value);
        }
        let result = match &request.body {
            Some(body) => call.send_bytes(body),
            None => call.call(),
        };
        let response = match result {
            Ok(response) | Err(ureq::Error::Status(_, response)) => response,
            Err(ureq::Error::Transport(error)) => {
                log::debug(
                    "http",
                    &format!(
                        "{} {} failed: {}",
                        request.method,
                        endpoint(&request.url),
                        error.kind()
                    ),
                );
                return Err(ProviderError::ConnectionFailed);
            }
        };
        let status = response.status();
        let mut headers = BTreeMap::new();
        for name in response.headers_names() {
            if let Some(value) = response.header(&name) {
                headers.insert(name.to_ascii_lowercase(), value.to_string());
            }
        }
        let mut body = Vec::new();
        response
            .into_reader()
            .take(32 * 1024 * 1024)
            .read_to_end(&mut body)
            .map_err(|_| ProviderError::ConnectionFailed)?;
        log::debug(
            "http",
            &format!(
                "{} {} -> {} ({} ms)",
                request.method,
                endpoint(&request.url),
                status,
                start.elapsed().as_millis()
            ),
        );
        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }
}

/// Host and path only: query strings can carry tokens.
fn endpoint(url: &str) -> String {
    url::Url::parse(url)
        .map(|url| format!("{}{}", url.host_str().unwrap_or(""), url.path()))
        .unwrap_or_default()
}
