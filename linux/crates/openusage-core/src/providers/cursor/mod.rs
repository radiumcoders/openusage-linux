//! Cursor usage, history, and sign-in. Ports `Providers/Cursor/` and `CursorLogin`.

mod auth;
mod client;
mod events;
mod history;
mod login;
mod mapper;
mod parse;
mod summary;

use serde_json::{Map, Value};

use crate::error::ProviderError;
use crate::http::HttpResponse;
use crate::kind::ProviderKind;
use crate::metric::{MetricLine, ProviderUsage};
use crate::provider::{FetchContext, Login, LoginMethod, Provider};
use crate::refresh::SECONDARY_READ_INTERVAL_SECONDS;

use mapper::{
    membership_type, resolved_plan_label, resolved_plan_name, stripe_balance_cents, PlanFacts,
};

const PLAN_KEY: &str = "cursor_plan";
const PLAN_READ: &str = "cursor_plan_read";
const MEMBERSHIP_KEY: &str = "cursor_membership";
const MEMBERSHIP_READ: &str = "cursor_membership_read";

pub fn provider() -> Box<dyn Provider> {
    Box::new(CursorProvider)
}

pub fn login(_method: &LoginMethod) -> Box<dyn Login> {
    Box::new(login::CursorLogin)
}

struct CursorProvider;

impl Provider for CursorProvider {
    fn fetch(&self, ctx: &mut FetchContext) -> Result<ProviderUsage, ProviderError> {
        let mut tokens = auth::prepare(ctx)?;
        let mut usage_response = client::fetch_usage(ctx.http, &tokens.access_token)?;
        if ProviderError::is_auth_failure(&usage_response) {
            tokens = auth::refresh(ctx, &tokens)?;
            usage_response = client::fetch_usage(ctx.http, &tokens.access_token)?;
        }
        ProviderError::require_success(&usage_response, ProviderKind::Cursor)?;
        let usage = json_object(&usage_response).ok_or(ProviderError::InvalidResponse)?;
        assemble(ctx, &usage, &tokens.access_token)
    }
}

fn assemble(
    ctx: &mut FetchContext,
    usage: &Map<String, Value>,
    access_token: &str,
) -> Result<ProviderUsage, ProviderError> {
    let account_id = ctx.account.card_id();
    let mut summary_read = false;
    let mut summary: Option<Map<String, Value>> = None;
    let mut read_summary = |ctx: &mut FetchContext| -> Option<Map<String, Value>> {
        if !summary_read {
            summary_read = true;
            summary = client::optional_json(
                &account_id,
                "usage-summary",
                client::fetch_summary(ctx.http, access_token),
            );
        }
        summary.clone()
    };

    if ctx.state.is_due(
        PLAN_READ,
        SECONDARY_READ_INTERVAL_SECONDS,
        ctx.now,
        ctx.force,
    ) {
        let fresh = client::optional_json(
            &account_id,
            "plan",
            client::fetch_plan(ctx.http, access_token).map(Some),
        )
        .as_ref()
        .and_then(client::plan_name);
        if let Some(name) = fresh {
            ctx.state.set(PLAN_KEY, &name);
        }
        ctx.state.mark_read(PLAN_READ, ctx.now);
    }
    if ctx.state.is_due(
        MEMBERSHIP_READ,
        SECONDARY_READ_INTERVAL_SECONDS,
        ctx.now,
        ctx.force,
    ) {
        let fresh = read_summary(ctx).as_ref().and_then(membership_type);
        if let Some(membership) = fresh {
            ctx.state.set(MEMBERSHIP_KEY, &membership);
        }
        ctx.state.mark_read(MEMBERSHIP_READ, ctx.now);
    }

    let plan_name = ctx.state.get::<String>(PLAN_KEY);
    let membership = ctx.state.get::<String>(MEMBERSHIP_KEY);
    let is_org = membership
        .as_deref()
        .is_some_and(|membership| mapper::ORG_MEMBERSHIPS.contains(&membership));
    let display_plan = resolved_plan_name(plan_name.as_deref(), membership.as_deref());
    let facts = PlanFacts::from_usage(usage);
    let fallback_plan = if is_org {
        membership.clone()
    } else {
        plan_name.clone()
    };
    let fallback = mapper::should_use_request_fallback(&facts, fallback_plan.as_deref());

    let mut mapped = if fallback.should {
        summary::map(
            read_summary(ctx).as_ref(),
            client::optional_json(
                &account_id,
                "request-based usage",
                client::fetch_requests(ctx.http, access_token),
            )
            .as_ref(),
            display_plan.as_deref(),
            &fallback.message,
        )?
    } else if facts.should_try_generic_request_fallback() {
        let request = client::optional_json(
            &account_id,
            "request-based usage",
            client::fetch_requests(ctx.http, access_token),
        );
        match request.as_ref() {
            Some(request) => match summary::map(None, Some(request), display_plan.as_deref(), "") {
                Ok(mapped) => mapped,
                Err(_) => primary(
                    ctx,
                    usage,
                    access_token,
                    plan_name.as_deref(),
                    membership.as_deref(),
                    &facts,
                    is_org,
                    &mut read_summary,
                )?,
            },
            None => primary(
                ctx,
                usage,
                access_token,
                plan_name.as_deref(),
                membership.as_deref(),
                &facts,
                is_org,
                &mut read_summary,
            )?,
        }
    } else {
        primary(
            ctx,
            usage,
            access_token,
            plan_name.as_deref(),
            membership.as_deref(),
            &facts,
            is_org,
            &mut read_summary,
        )?
    };

    if let Some(grok) = client::optional_json(
        &account_id,
        "Grok Bot usage",
        client::fetch_grok(ctx.http, access_token).map(Some),
    ) {
        if let Some(line) = mapper::map_grok(&grok) {
            mapped.lines.push(line);
        }
    }
    history::append_lines(ctx, access_token, &mut mapped);
    Ok(mapped)
}

fn primary(
    ctx: &mut FetchContext,
    usage: &Map<String, Value>,
    access_token: &str,
    plan_name: Option<&str>,
    membership: Option<&str>,
    facts: &PlanFacts,
    is_org: bool,
    read_summary: &mut impl FnMut(&mut FetchContext) -> Option<Map<String, Value>>,
) -> Result<ProviderUsage, ProviderError> {
    let account_id = ctx.account.card_id();
    let stripe = client::optional_json(
        &account_id,
        "prepaid-balance",
        client::fetch_stripe(ctx.http, access_token),
    );
    let grants = client::optional_json(
        &account_id,
        "credit-grants",
        client::fetch_credits(ctx.http, access_token).map(Some),
    );
    let mut mapped = mapper::map_usage(
        usage,
        plan_name,
        grants.as_ref(),
        stripe_balance_cents(stripe.as_ref()),
    )?;
    mapped.plan = resolved_plan_label(plan_name, membership);
    let needs_on_demand = !mapped
        .lines
        .iter()
        .any(|line: &MetricLine| line.label() == "On-demand");
    if is_org && needs_on_demand || !is_org && facts.is_team_by_shape {
        if let Some(summary) = read_summary(ctx) {
            if summary::has_usable_summary(&summary) {
                summary::append_missing_on_demand(&summary, &mut mapped.lines);
                let fresh = membership_type(&summary).or_else(|| membership.map(str::to_string));
                mapped.plan = resolved_plan_label(plan_name, fresh.as_deref()).or(mapped.plan);
            }
        }
    }
    Ok(mapped)
}

fn json_object(response: &HttpResponse) -> Option<Map<String, Value>> {
    response.json().and_then(|value| value.as_object().cloned())
}

#[cfg(test)]
mod tests;
