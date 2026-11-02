//! Port of `DevinUsageClient.swift`.

use std::time::Duration;

use serde_json::json;

use crate::account::OAuthTokens;
use crate::error::ProviderError;
use crate::http::{HttpClient, HttpRequest, HttpResponse};

pub(crate) const DEFAULT_SERVER: &str = "https://server.codeium.com";
pub(crate) const API_SERVER_KEY: &str = "api_server_url";
const VERSION: &str = "1.108.2";

pub(crate) fn fetch_user_status(
    http: &dyn HttpClient,
    tokens: &OAuthTokens,
) -> Result<HttpResponse, ProviderError> {
    let server = tokens
        .extras
        .get(API_SERVER_KEY)
        .and_then(|raw| url::Url::parse(raw).ok())
        .map(|url| url.to_string())
        .unwrap_or_else(|| DEFAULT_SERVER.to_string());
    let server = server.trim_end_matches('/');
    let mut request = HttpRequest::post(format!(
        "{server}/exa.seat_management_pb.SeatManagementService/GetUserStatus"
    ))
    .header("Connect-Protocol-Version", "1")
    .json_body(&json!({
        "metadata": {
            "apiKey": tokens.access_token,
            "ideName": "devin",
            "ideVersion": VERSION,
            "extensionName": "devin",
            "extensionVersion": VERSION,
            "locale": "en"
        }
    }));
    request.timeout = Duration::from_secs(15);
    http.send(request)
}
