//! Port of `CopilotUsageClient.swift`.

use std::time::Duration;

use crate::error::ProviderError;
use crate::http::{HttpClient, HttpRequest, HttpResponse};

use super::mapper;

pub(crate) const USAGE_URL: &str = "https://api.github.com/copilot_internal/user";

pub(crate) fn fetch_usage(
    http: &dyn HttpClient,
    token: &str,
) -> Result<HttpResponse, ProviderError> {
    let mut request = HttpRequest::get(USAGE_URL)
        .header("Authorization", format!("token {token}"))
        .header("Accept", "application/json")
        .header("Editor-Version", "vscode/1.96.2")
        .header("Editor-Plugin-Version", "copilot-chat/0.26.7")
        .header("User-Agent", "GitHubCopilotChat/0.26.7")
        .header("X-Github-Api-Version", "2025-04-01");
    request.timeout = Duration::from_secs(15);
    http.send(request)
}

pub(crate) fn organization_usage_url(organization: &str) -> String {
    let mut url =
        url::Url::parse("https://api.github.com/organizations/x/settings/billing/ai_credit/usage")
            .expect("org usage url");
    url.path_segments_mut()
        .expect("org usage path")
        .clear()
        .extend([
            "organizations",
            organization,
            "settings",
            "billing",
            "ai_credit",
            "usage",
        ]);
    url.to_string()
}

pub(crate) fn organization_usage(
    http: &dyn HttpClient,
    organization: &str,
    token: &str,
) -> Result<Vec<crate::metric::MetricLine>, ProviderError> {
    let mut request = HttpRequest::get(organization_usage_url(organization))
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "OpenUsage")
        .header("X-GitHub-Api-Version", "2022-11-28");
    request.timeout = Duration::from_secs(15);
    let response = http.send(request)?;
    if ProviderError::is_auth_failure(&response) {
        return Err(ProviderError::KeyRejected(
            "GitHub rejected the token.".into(),
        ));
    }
    if response.status == 404 {
        return Err(ProviderError::Unavailable(format!(
            "GitHub has no organization \u{201C}{organization}\u{201D} this token can read."
        )));
    }
    if !response.is_success() {
        return Err(ProviderError::failed(&response));
    }
    mapper::usage_lines(&response.body).ok_or(ProviderError::InvalidResponse)
}
