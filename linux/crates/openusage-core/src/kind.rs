use serde::{Deserialize, Serialize};

/// The providers OpenUsage reads. Raw values match the Mac app's `ProviderKind`, so stored accounts carry over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    Claude,
    Codex,
    Cursor,
    Antigravity,
    Copilot,
    Devin,
    Grok,
    Ollama,
    #[serde(rename = "opencode")]
    OpenCode,
    #[serde(rename = "openrouter")]
    OpenRouter,
    Zai,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthMethod {
    OAuth,
    ApiKey { key_page: &'static str },
    OrganizationToken,
}

pub struct ProviderLink {
    pub label: &'static str,
    pub url: &'static str,
}

impl ProviderKind {
    pub const ALL: [ProviderKind; 11] = [
        Self::Claude,
        Self::Codex,
        Self::Cursor,
        Self::Antigravity,
        Self::Copilot,
        Self::Devin,
        Self::Grok,
        Self::Ollama,
        Self::OpenCode,
        Self::OpenRouter,
        Self::Zai,
    ];

    pub fn raw(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Cursor => "cursor",
            Self::Antigravity => "antigravity",
            Self::Copilot => "copilot",
            Self::Devin => "devin",
            Self::Grok => "grok",
            Self::Ollama => "ollama",
            Self::OpenCode => "opencode",
            Self::OpenRouter => "openrouter",
            Self::Zai => "zai",
        }
    }

    pub fn from_raw(raw: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.raw() == raw.to_ascii_lowercase())
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Claude => "Claude",
            Self::Codex => "ChatGPT",
            Self::Cursor => "Cursor",
            Self::Antigravity => "Antigravity",
            Self::Copilot => "Copilot",
            Self::Devin => "Devin",
            Self::Grok => "Grok",
            Self::Ollama => "Ollama",
            Self::OpenCode => "OpenCode",
            Self::OpenRouter => "OpenRouter",
            Self::Zai => "Z.ai",
        }
    }

    /// The first method is the account's default sign-in.
    pub fn auth_methods(self) -> Vec<AuthMethod> {
        match self {
            Self::Claude
            | Self::Codex
            | Self::Cursor
            | Self::Antigravity
            | Self::Devin
            | Self::Grok => vec![AuthMethod::OAuth],
            Self::Copilot => vec![AuthMethod::OAuth, AuthMethod::OrganizationToken],
            Self::Ollama => vec![AuthMethod::ApiKey {
                key_page: "https://ollama.com/settings/keys",
            }],
            Self::OpenCode => vec![AuthMethod::ApiKey {
                key_page: "https://opencode.ai/console",
            }],
            Self::OpenRouter => vec![AuthMethod::ApiKey {
                key_page: "https://openrouter.ai/settings/management-keys",
            }],
            Self::Zai => vec![AuthMethod::ApiKey {
                key_page: "https://z.ai/manage-apikey/apikey-list",
            }],
        }
    }

    pub fn links(self) -> Vec<ProviderLink> {
        let link = |label, url| ProviderLink { label, url };
        match self {
            Self::Claude => vec![
                link("Status Page", "https://status.claude.com/"),
                link("Usage Dashboard", "https://claude.ai/settings/usage"),
            ],
            Self::Codex => vec![
                link("Status Page", "https://status.openai.com/"),
                link("Usage Dashboard", "https://chatgpt.com/settings/usage"),
            ],
            Self::Cursor => vec![
                link("Status Page", "https://status.cursor.com/"),
                link("Usage Dashboard", "https://www.cursor.com/dashboard"),
            ],
            Self::Antigravity => vec![],
            Self::Copilot => vec![
                link("Status Page", "https://www.githubstatus.com/"),
                link("Usage Dashboard", "https://github.com/settings/billing"),
            ],
            Self::Devin => vec![link(
                "Usage Dashboard",
                "https://app.devin.ai/settings/plans",
            )],
            Self::Grok => vec![link("Usage Dashboard", "https://grok.com/?_s=usage")],
            Self::Ollama => vec![
                link("Usage Dashboard", "https://ollama.com/settings"),
                link("API Keys", "https://ollama.com/settings/keys"),
            ],
            Self::OpenCode => vec![
                link("Status Page", "https://status.opencode.ai/"),
                link("Usage Dashboard", "https://opencode.ai/console"),
            ],
            Self::OpenRouter => vec![
                link("Status Page", "https://status.openrouter.ai/"),
                link("Usage Dashboard", "https://openrouter.ai/activity"),
                link("Credits", "https://openrouter.ai/settings/credits"),
            ],
            Self::Zai => vec![
                link(
                    "Usage Dashboard",
                    "https://z.ai/manage-apikey/coding-plan/personal/my-plan",
                ),
                link("API Keys", "https://z.ai/manage-apikey/apikey-list"),
            ],
        }
    }
}
