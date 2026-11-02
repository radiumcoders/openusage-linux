//! `~/.openusage/config.json`, the same file and format as the Mac app:
//! `{"proxy": {"enabled": true, "url": "socks5://host:port"}}`.

use serde::Deserialize;

use crate::log;

#[derive(Debug, Clone, PartialEq)]
pub struct ProxyConfig {
    pub scheme: String,
    pub host: String,
    pub port: u16,
    pub credential: Option<(String, String)>,
}

impl ProxyConfig {
    pub fn ureq_url(&self) -> String {
        let auth = self
            .credential
            .as_ref()
            .map(|(user, pass)| {
                format!(
                    "{}:{}@",
                    crate::http::form_encode(user),
                    crate::http::form_encode(pass)
                )
            })
            .unwrap_or_default();
        format!("{}://{}{}:{}", self.scheme, auth, self.host, self.port)
    }
}

#[derive(Deserialize)]
struct File {
    proxy: Option<Proxy>,
}

#[derive(Deserialize)]
struct Proxy {
    enabled: bool,
    url: Option<String>,
}

/// Error text never quotes the URL: a password can sit anywhere in it.
pub fn parse(data: &[u8]) -> Result<Option<ProxyConfig>, &'static str> {
    let file: File = serde_json::from_slice(data)
        .map_err(|_| r#"~/.openusage/config.json is not valid JSON in the form {"proxy": {"enabled": true, "url": "socks5://host:port"}}"#)?;
    let Some(proxy) = file.proxy.filter(|proxy| proxy.enabled) else {
        return Ok(None);
    };
    let invalid = "proxy.url in ~/.openusage/config.json must be socks5:// or http://, then a host and an optional port";
    let url = proxy
        .url
        .as_deref()
        .and_then(|url| url::Url::parse(url).ok())
        .ok_or(invalid)?;
    let scheme = url.scheme().to_ascii_lowercase();
    let default_port = match scheme.as_str() {
        "socks5" => 1080,
        "http" => 80,
        _ => return Err(invalid),
    };
    let host = url
        .host_str()
        .filter(|host| !host.is_empty())
        .ok_or(invalid)?
        .to_string();
    let credential = (!url.username().is_empty()).then(|| {
        let decode = |value: &str| {
            url::form_urlencoded::parse(format!("v={value}").as_bytes())
                .next()
                .map(|(_, v)| v.into_owned())
                .unwrap_or_default()
        };
        (decode(url.username()), decode(url.password().unwrap_or("")))
    });
    Ok(Some(ProxyConfig {
        scheme,
        host,
        port: url.port().unwrap_or(default_port),
        credential,
    }))
}

pub fn load() -> Option<ProxyConfig> {
    let path = crate::paths::home().join(".openusage/config.json");
    let data = match std::fs::read(&path) {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            log::error("config", &format!("proxy off: {error}"));
            return None;
        }
    };
    match parse(&data) {
        Ok(Some(proxy)) => {
            log::notice(
                "config",
                &format!("proxy on: {}://{}:{}", proxy.scheme, proxy.host, proxy.port),
            );
            Some(proxy)
        }
        Ok(None) => None,
        Err(message) => {
            log::error("config", &format!("proxy off: {message}"));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_socks_and_http_and_ignores_disabled() {
        let socks = parse(br#"{"proxy":{"enabled":true,"url":"socks5://u:p%40ss@127.0.0.1"}}"#)
            .unwrap()
            .unwrap();
        assert_eq!(
            (socks.port, socks.credential.clone()),
            (1080, Some(("u".into(), "p@ss".into())))
        );
        assert_eq!(
            parse(br#"{"proxy":{"enabled":false,"url":"http://h"}}"#).unwrap(),
            None
        );
        assert!(parse(br#"{"proxy":{"enabled":true,"url":"ftp://h"}}"#).is_err());
    }
}
