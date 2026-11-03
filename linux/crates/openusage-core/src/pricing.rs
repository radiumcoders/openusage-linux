//! Model prices for local spend estimates, a port of 0.7.x's `ModelPricing` with the same bundled
//! catalogs: OpenUsage's pricing supplement and a LiteLLM snapshot. Rates are USD per million tokens.
//!
//! Resolution for a model name, as on the Mac:
//! 1. A supplement alias rule rewrites the slug to a canonical key (the raw name stays a fallback).
//! 2. Supplement pricing, exact.
//! 3. LiteLLM, exact.
//! 4. A `-fast` slug prices its base model times its fast multiplier, or stays unpriced.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use regex::Regex;
use serde::Deserialize;

const SUPPLEMENT: &str = include_str!("../pricing/supplement.json");
const LITELLM: &str = include_str!("../pricing/litellm.json");
/// 1-hour cache writes bill at twice the input rate (ccusage's rule).
const CACHE_WRITE_1H_INPUT_MULTIPLIER: f64 = 2.0;
const LONG_CONTEXT_THRESHOLD: u64 = 200_000;

#[derive(Debug, Clone, PartialEq)]
pub struct Rates {
    pub input: f64,
    pub output: f64,
    pub cache_write: f64,
    pub cache_read: f64,
    pub input_above: Option<f64>,
    pub output_above: Option<f64>,
    pub cache_write_above: Option<f64>,
    pub cache_read_above: Option<f64>,
    pub fast_multiplier: f64,
}

/// Token counts split into the buckets that price differently.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tokens {
    pub input: u64,
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
    pub cache_read: u64,
    pub output: u64,
    pub fast: bool,
}

impl Tokens {
    pub fn prompt(&self) -> u64 {
        self.input + self.cache_write_5m + self.cache_write_1h + self.cache_read
    }

    pub fn total(&self) -> u64 {
        self.prompt() + self.output
    }
}

impl Rates {
    fn scaled(&self, factor: f64) -> Self {
        let scale = |value: Option<f64>| value.map(|rate| rate * factor);
        Self {
            input: self.input * factor,
            output: self.output * factor,
            cache_write: self.cache_write * factor,
            cache_read: self.cache_read * factor,
            input_above: scale(self.input_above),
            output_above: scale(self.output_above),
            cache_write_above: scale(self.cache_write_above),
            cache_read_above: scale(self.cache_read_above),
            fast_multiplier: 1.0,
        }
    }

    /// One request's dollars: the whole request takes the long-context rates once its prompt passes
    /// 200k tokens, and a fast request is scaled by the fast multiplier.
    pub fn cost(&self, tokens: &Tokens) -> f64 {
        self.cost_with(tokens, true)
    }

    /// `cost`, optionally without the long-context tier: daily totals (Cursor's) no longer know
    /// which request crossed it.
    pub fn cost_with(&self, tokens: &Tokens, long_context: bool) -> f64 {
        let long = long_context && tokens.prompt() > LONG_CONTEXT_THRESHOLD;
        let pick = |base: f64, above: Option<f64>| if long { above.unwrap_or(base) } else { base };
        let input = pick(self.input, self.input_above);
        let dollars = tokens.input as f64 * input
            + tokens.output as f64 * pick(self.output, self.output_above)
            + tokens.cache_write_5m as f64 * pick(self.cache_write, self.cache_write_above)
            + tokens.cache_write_1h as f64 * input * CACHE_WRITE_1H_INPUT_MULTIPLIER
            + tokens.cache_read as f64 * pick(self.cache_read, self.cache_read_above);
        let multiplier = if tokens.fast { self.fast_multiplier } else { 1.0 };
        dollars / 1_000_000.0 * multiplier
    }
}

#[derive(Deserialize)]
struct SupplementFile {
    pricing: HashMap<String, SupplementRates>,
    #[serde(default)]
    fast_multipliers: HashMap<String, f64>,
    #[serde(default)]
    alias_rules: Vec<AliasRule>,
}

#[derive(Deserialize)]
struct SupplementRates {
    input_per_million: f64,
    output_per_million: f64,
    cache_write_per_million: Option<f64>,
    cache_read_per_million: Option<f64>,
}

#[derive(Deserialize)]
struct AliasRule {
    pattern: String,
    canonical: String,
}

#[derive(Deserialize)]
struct LiteLlmFile {
    models: HashMap<String, LiteLlmRates>,
}

#[derive(Deserialize)]
struct LiteLlmRates {
    i: f64,
    o: f64,
    cw: Option<f64>,
    cr: Option<f64>,
    ia: Option<f64>,
    oa: Option<f64>,
    cwa: Option<f64>,
    cra: Option<f64>,
    fast: Option<f64>,
}

pub struct Pricing {
    supplement: HashMap<String, Rates>,
    litellm: HashMap<String, Rates>,
    fast_multipliers: HashMap<String, f64>,
    aliases: Vec<(Regex, String)>,
    memo: Mutex<HashMap<String, Option<Rates>>>,
}

impl Pricing {
    /// The catalogs bundled with this build.
    pub fn bundled() -> &'static Pricing {
        static PRICING: OnceLock<Pricing> = OnceLock::new();
        PRICING.get_or_init(|| Pricing::parse(SUPPLEMENT, LITELLM))
    }

    fn parse(supplement: &str, litellm: &str) -> Pricing {
        let supplement: SupplementFile = match serde_json::from_str(supplement) {
            Ok(file) => file,
            Err(error) => {
                crate::log::error("pricing", &format!("bundled supplement unreadable: {error}"));
                SupplementFile {
                    pricing: HashMap::new(),
                    fast_multipliers: HashMap::new(),
                    alias_rules: Vec::new(),
                }
            }
        };
        let litellm: HashMap<String, LiteLlmRates> = match serde_json::from_str::<LiteLlmFile>(litellm) {
            Ok(file) => file.models,
            Err(error) => {
                crate::log::error("pricing", &format!("bundled LiteLLM snapshot unreadable: {error}"));
                HashMap::new()
            }
        };
        let aliases = supplement
            .alias_rules
            .iter()
            .filter_map(|rule| match Regex::new(&rule.pattern) {
                Ok(regex) => Some((regex, rule.canonical.clone())),
                Err(error) => {
                    crate::log::debug("pricing", &format!("alias rule skipped: {error}"));
                    None
                }
            })
            .collect();
        Pricing {
            supplement: supplement
                .pricing
                .iter()
                .map(|(key, rates)| {
                    let rates = Rates {
                        input: rates.input_per_million,
                        output: rates.output_per_million,
                        cache_write: rates.cache_write_per_million.unwrap_or(rates.input_per_million),
                        cache_read: rates
                            .cache_read_per_million
                            .unwrap_or(rates.input_per_million * 0.1),
                        input_above: None,
                        output_above: None,
                        cache_write_above: None,
                        cache_read_above: None,
                        fast_multiplier: supplement.fast_multipliers.get(key).copied().unwrap_or(1.0),
                    };
                    (key.clone(), rates)
                })
                .collect(),
            litellm: litellm
                .into_iter()
                .map(|(key, rates)| {
                    let rates = Rates {
                        input: rates.i,
                        output: rates.o,
                        cache_write: rates.cw.unwrap_or(rates.i),
                        cache_read: rates.cr.unwrap_or(rates.i * 0.1),
                        input_above: rates.ia,
                        output_above: rates.oa,
                        cache_write_above: rates.cwa,
                        cache_read_above: rates.cra,
                        fast_multiplier: rates.fast.unwrap_or(1.0),
                    };
                    (key, rates)
                })
                .collect(),
            fast_multipliers: supplement.fast_multipliers,
            aliases,
            memo: Mutex::new(HashMap::new()),
        }
    }

    /// The canonical key an alias rule gives `model`, or `model` itself.
    pub fn canonical(&self, model: &str) -> String {
        self.aliases
            .iter()
            .find(|(regex, _)| regex.is_match(model))
            .map(|(_, canonical)| canonical.clone())
            .unwrap_or_else(|| model.to_string())
    }

    /// Rates for `model`, or none when nothing prices it.
    pub fn resolve(&self, model: &str) -> Option<Rates> {
        if let Some(cached) = self.memo.lock().ok().and_then(|memo| memo.get(model).cloned()) {
            return cached;
        }
        let canonical = self.canonical(model);
        let resolved = if canonical != model {
            self.lookup(&canonical).or_else(|| self.lookup(model))
        } else {
            self.lookup(model)
        };
        if let Ok(mut memo) = self.memo.lock() {
            memo.insert(model.to_string(), resolved.clone());
        }
        resolved
    }

    fn lookup(&self, name: &str) -> Option<Rates> {
        if let Some(rates) = self.supplement.get(name).or_else(|| self.litellm.get(name)) {
            return Some(rates.clone());
        }
        let base = name.strip_suffix("-fast").filter(|base| !base.is_empty())?;
        let rates = self.supplement.get(base).or_else(|| self.litellm.get(base))?;
        let multiplier = if rates.fast_multiplier != 1.0 {
            rates.fast_multiplier
        } else {
            *self.fast_multipliers.get(base)?
        };
        Some(rates.scaled(multiplier))
    }

    pub fn cost(&self, model: &str, tokens: &Tokens) -> Option<f64> {
        self.resolve(model).map(|rates| rates.cost(tokens))
    }

    /// `cost` for summed tokens, priced at the standard tier.
    pub fn cost_of_totals(&self, model: &str, tokens: &Tokens) -> Option<f64> {
        self.resolve(model).map(|rates| rates.cost_with(tokens, false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_models_price_like_the_mac_app() {
        let pricing = Pricing::bundled();
        let tokens = Tokens {
            input: 1_000_000,
            output: 1_000_000,
            ..Default::default()
        };
        // Opus 5.5: $4 in, $20 out.
        assert_eq!(pricing.cost("claude-opus-5-5", &tokens), Some(24.0));
        // An alias rule maps the 1M-context slug onto the same entry.
        assert_eq!(pricing.cost("claude-opus-5-5[1m]", &tokens), Some(24.0));
        // Fast mode doubles it.
        let fast = Tokens { fast: true, ..tokens };
        assert_eq!(pricing.cost("claude-opus-5-5", &fast), Some(48.0));
        // 1-hour cache writes bill at twice input.
        let cache = Tokens {
            cache_write_1h: 1_000_000,
            ..Default::default()
        };
        assert_eq!(pricing.cost("claude-opus-5-5", &cache), Some(8.0));
        assert!(pricing.resolve("claude-haiku-4-5-20251001").is_some());
        assert_eq!(pricing.resolve("not-a-model"), None);
    }
}
