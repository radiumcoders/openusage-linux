use std::fmt;

use openusage_core::kind::ProviderKind;
use openusage_core::paths;

use crate::notify::{self, Milestone};
use crate::waybar::DEFAULT_ICON;

#[derive(Debug)]
pub struct UsageError {
    message: String,
}

impl UsageError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlertChange {
    pub on: bool,
    pub milestones: Vec<Milestone>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Help,
    Read {
        token: Option<String>,
        force: bool,
    },
    Accounts,
    Add {
        provider: String,
        organization: Option<String>,
    },
    Remove {
        account: String,
    },
    Rename {
        account: String,
        name: String,
    },
    Refresh,
    Waybar {
        once: bool,
        icon: String,
    },
    WaybarRefresh,
    Alerts(Option<AlertChange>),
    Panel,
    HubList,
    HubAdd {
        url: String,
    },
    HubRemove {
        hub: String,
    },
}

pub fn help() -> String {
    let names = ProviderKind::ALL
        .iter()
        .map(|kind| kind.raw())
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "\
Usage: openusage [--force] [provider|account-id]
       openusage accounts
       openusage add <provider> [--org <name>]
       openusage remove <account-id>
       openusage rename <account-id> <name>
       openusage refresh
       openusage waybar [--once] [--icon <text>]
       openusage waybar --refresh
       openusage alerts [on|off [under-10|under-30|running-out ...]]
       openusage panel
       openusage hub list
       openusage hub add <url>
       openusage hub remove <hub-id-or-host>

With no command, print the usage last fetched as openusage.limits.v2 JSON.

  provider     An account id (codex@1a2b3c4d) or a provider kind
               ({names}).

Commands:
  accounts     List accounts: id, provider, label, and plan when one is cached.
               A CLIProxyAPI account's label includes its hub host.
  hub          Connect a CLIProxyAPI hub, list hubs, or remove one.
               hub add reads the management key with echo off.
  add          Sign in. Browser sign-in opens the URL and prints it. API keys and
               tokens are read from stdin with echo off in a terminal. --org adds a
               Copilot organization by its token.
  remove       Remove an account and its saved sign-in.
  rename       Rename an account.
  refresh      Fetch every account now.
  waybar       Print Waybar JSON lines. Refreshes on start and every 5 minutes.
               --once prints the cache and exits. --icon is the text used when
               nothing is running out (default: a gauge glyph). Left click opens
               the panel. Right click, or `openusage waybar --refresh`, asks the
               running module to refresh every account now (SIGUSR1).
  alerts       List the desktop notification switches, or turn them on or off
               (all of them when none is named). All start off.
  panel        Open or close the usage panel.

Files:
  {settings}
  {snapshots}
  {pid}

Options:
  --force      Accepted when printing usage, then ignored.
  -h, --help   Show this help.

Exit codes: 0 printed or completed, 2 bad arguments or an unknown provider,
4 nothing fetched yet. A failed refresh exits 1. Ctrl-C during sign-in exits 130.",
        settings = paths::settings_file().display(),
        snapshots = paths::snapshots_file().display(),
        pid = paths::daemon_pid_file().display(),
    )
}

pub fn parse(arguments: &[String]) -> Result<Command, UsageError> {
    if arguments
        .iter()
        .any(|argument| argument == "-h" || argument == "--help")
    {
        return Ok(Command::Help);
    }
    let mut index = 0;
    let mut force = false;
    let mut once = false;
    let mut organization = None;
    let mut icon = None;
    let mut waybar_refresh = false;
    let mut positionals = Vec::new();
    while index < arguments.len() {
        let argument = &arguments[index];
        match argument.as_str() {
            "--force" => force = true,
            "--once" => once = true,
            "--refresh" => waybar_refresh = true,
            "--org" => {
                index += 1;
                if index >= arguments.len() || arguments[index].starts_with('-') {
                    return Err(UsageError::new("--org needs an organization name."));
                }
                organization = Some(arguments[index].clone());
            }
            "--icon" => {
                index += 1;
                if index >= arguments.len() || arguments[index].is_empty() {
                    return Err(UsageError::new("--icon needs a value."));
                }
                icon = Some(arguments[index].clone());
            }
            option if option.starts_with('-') => {
                return Err(UsageError::new(format!("Unknown option: {option}")))
            }
            _ => positionals.push(argument.clone()),
        }
        index += 1;
    }

    if waybar_refresh && positionals.first().map(String::as_str) != Some("waybar") {
        return Err(UsageError::new("--refresh only applies to waybar."));
    }
    let Some(command) = positionals.first().cloned() else {
        unexpected(
            force,
            once,
            organization.is_some(),
            icon.is_some(),
            true,
            false,
            false,
            false,
        )?;
        return Ok(Command::Read { token: None, force });
    };
    match command.as_str() {
        "help" => Ok(Command::Help),
        "accounts" => {
            if positionals.len() != 1 {
                return Err(UsageError::new("accounts takes no arguments."));
            }
            unexpected(
                force,
                once,
                organization.is_some(),
                icon.is_some(),
                false,
                false,
                false,
                false,
            )?;
            Ok(Command::Accounts)
        }
        "add" => {
            if positionals.len() != 2 {
                return Err(UsageError::new("add needs a provider."));
            }
            unexpected(
                force,
                once,
                organization.is_some(),
                icon.is_some(),
                false,
                false,
                true,
                false,
            )?;
            Ok(Command::Add {
                provider: positionals[1].clone(),
                organization,
            })
        }
        "remove" => {
            if positionals.len() != 2 {
                return Err(UsageError::new("remove needs an account id."));
            }
            unexpected(
                force,
                once,
                organization.is_some(),
                icon.is_some(),
                false,
                false,
                false,
                false,
            )?;
            Ok(Command::Remove {
                account: positionals[1].clone(),
            })
        }
        "rename" => {
            if positionals.len() < 3 {
                return Err(UsageError::new("rename needs an account id and a name."));
            }
            unexpected(
                force,
                once,
                organization.is_some(),
                icon.is_some(),
                false,
                false,
                false,
                false,
            )?;
            Ok(Command::Rename {
                account: positionals[1].clone(),
                name: positionals[2..].join(" "),
            })
        }
        "refresh" => {
            if positionals.len() != 1 {
                return Err(UsageError::new("refresh takes no arguments."));
            }
            unexpected(
                force,
                once,
                organization.is_some(),
                icon.is_some(),
                false,
                false,
                false,
                false,
            )?;
            Ok(Command::Refresh)
        }
        "alerts" => {
            unexpected(
                force,
                once,
                organization.is_some(),
                icon.is_some(),
                false,
                false,
                false,
                false,
            )?;
            if positionals.len() == 1 {
                return Ok(Command::Alerts(None));
            }
            if positionals[1] != "on" && positionals[1] != "off" {
                return Err(UsageError::new(
                    "alerts takes on or off, then optional alert names.",
                ));
            }
            let milestones = notify::parse_names(&positionals[2..]).map_err(UsageError::new)?;
            Ok(Command::Alerts(Some(AlertChange {
                on: positionals[1] == "on",
                milestones,
            })))
        }
        "waybar" => {
            if positionals.len() != 1 {
                return Err(UsageError::new("waybar takes no arguments."));
            }
            if waybar_refresh {
                if once || icon.is_some() {
                    return Err(UsageError::new("--refresh takes no other options."));
                }
                unexpected(
                    force,
                    false,
                    organization.is_some(),
                    false,
                    false,
                    false,
                    false,
                    false,
                )?;
                return Ok(Command::WaybarRefresh);
            }
            unexpected(
                force,
                once,
                organization.is_some(),
                icon.is_some(),
                false,
                true,
                false,
                true,
            )?;
            Ok(Command::Waybar {
                once,
                icon: icon.unwrap_or_else(|| DEFAULT_ICON.to_string()),
            })
        }
        "panel" => {
            if positionals.len() != 1 {
                return Err(UsageError::new("panel takes no arguments."));
            }
            unexpected(
                force,
                once,
                organization.is_some(),
                icon.is_some(),
                false,
                false,
                false,
                false,
            )?;
            Ok(Command::Panel)
        }
        "hub" => {
            unexpected(
                force,
                once,
                organization.is_some(),
                icon.is_some(),
                false,
                false,
                false,
                false,
            )?;
            match positionals.get(1).map(String::as_str) {
                Some("list") if positionals.len() == 2 => Ok(Command::HubList),
                Some("add") if positionals.len() == 3 => Ok(Command::HubAdd {
                    url: positionals[2].clone(),
                }),
                Some("remove") if positionals.len() == 3 => Ok(Command::HubRemove {
                    hub: positionals[2].clone(),
                }),
                Some("list") => Err(UsageError::new("hub list takes no arguments.")),
                Some("add") => Err(UsageError::new("hub add needs the hub's address.")),
                Some("remove") => Err(UsageError::new("hub remove needs a hub id or host.")),
                _ => Err(UsageError::new("hub needs add, list, or remove.")),
            }
        }
        _ => {
            if positionals.len() != 1 {
                return Err(UsageError::new(
                    "Only one provider can be requested at a time.",
                ));
            }
            unexpected(
                force,
                once,
                organization.is_some(),
                icon.is_some(),
                true,
                false,
                false,
                false,
            )?;
            Ok(Command::Read {
                token: Some(command),
                force,
            })
        }
    }
}

fn unexpected(
    force: bool,
    once: bool,
    org: bool,
    icon: bool,
    allow_force: bool,
    allow_once: bool,
    allow_org: bool,
    allow_icon: bool,
) -> Result<(), UsageError> {
    if force && !allow_force {
        return Err(UsageError::new("--force only applies when printing usage."));
    }
    if once && !allow_once {
        return Err(UsageError::new("--once only applies to waybar."));
    }
    if org && !allow_org {
        return Err(UsageError::new("--org only applies to add."));
    }
    if icon && !allow_icon {
        return Err(UsageError::new("--icon only applies to waybar."));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_string).collect()
    }

    fn parsed(text: &str) -> Command {
        parse(&args(text)).unwrap()
    }

    fn failed(text: &str) -> String {
        parse(&args(text)).err().unwrap().to_string()
    }

    #[test]
    fn parses_the_command_surface() {
        assert_eq!(
            parsed(""),
            Command::Read {
                token: None,
                force: false
            }
        );
        assert_eq!(
            parsed("--force"),
            Command::Read {
                token: None,
                force: true
            }
        );
        assert_eq!(
            parsed("--force codex"),
            Command::Read {
                token: Some("codex".into()),
                force: true
            }
        );
        assert_eq!(
            parsed("codex@1a2b3c4d"),
            Command::Read {
                token: Some("codex@1a2b3c4d".into()),
                force: false
            }
        );
        assert_eq!(parsed("--help"), Command::Help);
        assert_eq!(parsed("accounts -h"), Command::Help);
        assert_eq!(parsed("help"), Command::Help);
        assert_eq!(parsed("accounts"), Command::Accounts);
        assert_eq!(
            parsed("add claude"),
            Command::Add {
                provider: "claude".into(),
                organization: None
            }
        );
        assert_eq!(
            parsed("add copilot --org acme"),
            Command::Add {
                provider: "copilot".into(),
                organization: Some("acme".into())
            }
        );
        assert_eq!(
            parsed("remove codex@1"),
            Command::Remove {
                account: "codex@1".into()
            }
        );
        assert_eq!(
            parsed("rename codex@1 My Work"),
            Command::Rename {
                account: "codex@1".into(),
                name: "My Work".into()
            }
        );
        assert_eq!(parsed("refresh"), Command::Refresh);
        assert_eq!(parsed("panel"), Command::Panel);
        assert_eq!(parsed("hub list"), Command::HubList);
        assert_eq!(
            parsed("hub add http://mini.local:8317"),
            Command::HubAdd {
                url: "http://mini.local:8317".into()
            }
        );
        assert_eq!(
            parsed("hub remove mini.local:8317"),
            Command::HubRemove {
                hub: "mini.local:8317".into()
            }
        );
        assert_eq!(parsed("waybar --refresh"), Command::WaybarRefresh);
        assert_eq!(
            parsed("waybar --once --icon X"),
            Command::Waybar {
                once: true,
                icon: "X".into()
            }
        );
        assert_eq!(parsed("waybar").to_owned(), parsed("waybar"));
        match parsed("waybar") {
            Command::Waybar { once: false, icon } => assert_eq!(icon, DEFAULT_ICON),
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(
            parsed("alerts on under-10"),
            Command::Alerts(Some(AlertChange {
                on: true,
                milestones: vec![Milestone::UnderTen]
            }))
        );
        assert_eq!(
            parsed("alerts off"),
            Command::Alerts(Some(AlertChange {
                on: false,
                milestones: Milestone::ALL.to_vec()
            }))
        );
        assert_eq!(parsed("alerts"), Command::Alerts(None));
    }

    #[test]
    fn rejects_unknown_flags_and_mismatched_arguments() {
        assert_eq!(failed("--foo"), "Unknown option: --foo");
        assert_eq!(failed("accounts extra"), "accounts takes no arguments.");
        assert_eq!(failed("add"), "add needs a provider.");
        assert_eq!(
            failed("add claude --org"),
            "--org needs an organization name."
        );
        assert_eq!(failed("--org acme"), "--org only applies to add.");
        assert_eq!(
            failed("accounts --force"),
            "--force only applies when printing usage."
        );
        assert_eq!(failed("--once"), "--once only applies to waybar.");
        assert_eq!(failed("waybar --icon"), "--icon needs a value.");
        assert_eq!(
            failed("waybar --refresh --once"),
            "--refresh takes no other options."
        );
        assert_eq!(failed("--refresh"), "--refresh only applies to waybar.");
        assert_eq!(
            failed("codex extra"),
            "Only one provider can be requested at a time."
        );
        assert_eq!(
            failed("alerts maybe"),
            "alerts takes on or off, then optional alert names."
        );
        assert_eq!(
            failed("alerts on nope"),
            "Unknown alert: nope. Expected under-10, under-30, running-out."
        );
        assert_eq!(failed("panel extra"), "panel takes no arguments.");
        assert_eq!(
            failed("rename id"),
            "rename needs an account id and a name."
        );
        let help = help();
        assert!(help.contains("openusage.limits.v2"));
        assert!(help.contains("snapshots.json"));
        assert!(help.contains("daemon.pid"));
        assert!(help.contains("openusage panel"));
        assert!(help.contains("openusage hub add"));
        assert_eq!(failed("hub"), "hub needs add, list, or remove.");
        assert_eq!(failed("hub add"), "hub add needs the hub's address.");
        assert_eq!(failed("hub list extra"), "hub list takes no arguments.");
        assert_eq!(failed("hub remove"), "hub remove needs a hub id or host.");
        assert_eq!(
            failed("hub add http://mini.local:8317 --force"),
            "--force only applies when printing usage."
        );
    }
}
