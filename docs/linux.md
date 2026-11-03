# Linux

OpenUsage on Linux is the Rust edition of the Mac app: the same providers and sign-ins, a Waybar
module, a GTK panel, and the `openusage` command. A Swift toolchain is not needed to build or run it.

## Requirements

- Rust and Cargo, to build.
- `secret-tool` (libsecret), and a Secret Service keyring such as GNOME Keyring, or KWallet with
  Secret Service turned on.
- `notify-send` (libnotify), for quota alerts.
- `xdg-open`, so a browser sign-in can open its link.
- GTK 4 and `gtk4-layer-shell`, for the panel.
- A Nerd Font, for the default Waybar icon. Pass `--icon` to use other text.

## Install

```sh
script/install_linux.sh
```

The script runs `cargo build --release --manifest-path linux/Cargo.toml` and installs `openusage` and
`openusage-panel` to `~/.local/bin`. It warns if `secret-tool`, `notify-send`, `xdg-open`, or
`gtk4-layer-shell` is missing. `~/.local/bin` has to be on `PATH`.

To build without installing:

```sh
cargo build --release --manifest-path linux/Cargo.toml
```

## Panel

`openusage panel`, or `openusage-panel --toggle`, opens and closes the usage panel. Waybar's left
click runs that. The panel reads the same accounts, snapshots, and refresh status as the bar.

All Usage opens with **Total Spend**, as in 0.7.x on the Mac: a Today / Yesterday / 30 Days switch
over a donut of each provider's share of the spend, summed over its accounts, with the total in the
middle and each provider's dollars beside it. The ⓘ names the providers it includes, and the share
button copies the card as an image. A period with no spend says so instead of drawing an empty ring.
**Show Total Spend** in Settings turns it off. The period is saved under
`openusage.totalSpend.period`. Hidden history rows are left out.

The dollars are estimates at API prices, priced with the catalogs 0.7.x bundled (OpenUsage's pricing
supplement and a LiteLLM snapshot), the same way 0.7.x priced them:

- **Claude**: Today, Yesterday, and Last 30 Days come from Claude Code's own session logs, every
  `projects/**/*.jsonl` under `$CLAUDE_CONFIG_DIR` (or `~/.config/claude` and `~/.claude`). Lines are
  deduplicated by message and request, advisor calls count under their own model, a line's own
  `costUSD` wins, and a model nothing prices is left out. The logs belong to the Claude account
  Claude Code is signed in to (`oauthAccount` in its `.claude.json`). Files are read once and then
  only from where they grew.
- **Cursor**: each model's tokens from Cursor's usage events, priced the same way. A model the
  catalogs don't name keeps what Cursor charged.
- Other providers that report history rows (ChatGPT, Ollama, OpenCode) use the dollars they report.

Scrolled content fades out at the tabs and the footer, as on the Mac. A mouse wheel scrolls smoothly; a
touchpad scrolls as GTK does. A page that refreshes while you scroll waits until you stop, and keeps
its place.

The **⋯** menu opens **Accounts** and **Settings** (Return opens Accounts from the dashboard). Accounts
lists every account in the saved dashboard order. A hidden metric is left off the dashboard, the bar,
and quota alerts. The limits JSON still includes it, as on the Mac.

An account's page renames it, removes it, and offers **Sign In Again** when that account has a problem. **Add New Account** signs in from the panel: pick a provider,
and for Copilot choose a personal seat or organization billing. A browser sign-in has **Open Browser**
and a code field; a device code is shown large with **Copy** and **Open**; an API key is a hidden
field with a link to the key page. **Needs Attention** uses **Fix** to open the account. Right-click a provider's card for
**Refresh**, **Manage Accounts…**, **Share Screenshot**, and its links; right-click an account for
**Refresh**, **Rename**, **Edit Account…**, **Share Screenshot**, and, for a Copilot organization,
**Organization Billing**. **Share Screenshot** copies a branded image of the card to the clipboard.
On All Usage, drag a provider by its header, an account by its name, or a metric by its row; on
Accounts, drag an account by its handle. An account's page lists its metrics with a switch to hide
each one everywhere and a handle to reorder it. **Ctrl+Z** undoes the last reorder or hide. The order
and hidden metrics are saved under the Mac key `openusage.layout.v2`. An account that is the only one of its provider starts from that provider's 0.7.x layout when one is saved (the read-only `openusage.layout.v1` keys), as on the Mac. Settings stores the
Mac keys for quota alerts, how many limits the bar shows, used or left, and countdown or exact reset
times. Clicking an amount or a reset on the dashboard flips the same two settings. A **Limit Resets** row has
**Claim** when a reset can be used now (Claude and ChatGPT), and a history row shows its top models
when the provider reports them.

## Add an account

From the panel, **Accounts → Add New Account**. Or from a terminal:

```sh
openusage add claude
openusage add copilot --org my-org
```

A terminal sign-in prints the link and opens it with `xdg-open`. A device-code sign-in prints the code
to enter. An API-key sign-in reads the key from stdin with echo off in a terminal. The provider ids
match the Mac app: `claude`, `codex`, `cursor`, `antigravity`, `copilot`, `devin`, `grok`, `ollama`,
`opencode`, `openrouter`, `zai`. What each sign-in grants is in [Providers](providers.md).

`--org <name>` adds a Copilot organization's billing as its own account, signed in with an
organization token.

## CLIProxyAPI

A CLIProxyAPI hub is not a provider. `openusage hub add <url>` asks for the hub's management key
with echo off, checks it, and mirrors each supported login as an ordinary account. Claude, ChatGPT,
Antigravity, Devin, and Grok logins are mirrored. The account's saved sign-in is the placeholder
`$TOKEN$`; the hub's key is kept in the keyring under the hub's id. Usage reads go through the hub's
management API, one at a time. A rejected key or an IP ban stops further calls to that hub until it
is removed and added again.

`openusage hub list` prints each hub's id, host, and address. `openusage hub remove <hub-id-or-host>`
removes the hub, its key, and the accounts mirrored from it. `openusage accounts` shows a mirrored
account with its hub host. The limits JSON `displayName` stays the account name, the same as on the
Mac (`Claude · alice@example.com`).

Hubs are stored under `cliproxyHubs.v1` in the settings property list, in the same JSON the Mac app
writes, so a hub list from the Mac app reads here.

## Waybar

Merge [contrib/waybar/config.jsonc](../contrib/waybar/config.jsonc) into the Waybar config, and add
`custom/openusage` to a modules list. Copy the classes in
[contrib/waybar/style.css](../contrib/waybar/style.css) into the Waybar stylesheet.

`openusage waybar` stays running and prints one JSON object per line (`return-type: json`) on each
update. It refreshes every 5 minutes. A left click opens the panel. A right click runs
`openusage waybar --refresh`, which asks the running module to refresh every account now (`SIGUSR1`).

`--once` prints one line from the saved snapshots and exits, without refreshing. When no limit is
running low, the module shows only an icon, as the Mac menu bar shows only the app icon;
`--icon <text>` replaces that icon (a Nerd Font gauge by default).

| Field | Meaning |
| --- | --- |
| `text` | The same strip as the Mac menu bar: the limits closest to running out, or the icon when none is. Hidden metrics are left out. |
| `tooltip` | Every account, one plain line per metric (`Session: 75% left · resets in 38m`), without hidden metrics. |
| `class` | `normal`, `warning`, or `critical`, plus `error` or `stale` when that is the state. No accounts is `empty`. |
| `percentage` | The fullest limit, as a percent used. |
| `alt` | The class names, for `{alt}`. |

## Omarchy bar

Omarchy's own bar has no Waybar, but it loads custom QML modules.
[contrib/omarchy/openusage.qml](../contrib/omarchy/openusage.qml) runs `openusage waybar` and shows the
same text and colors; like the Mac status item it has no hover tooltip. Left click opens the panel; right click refreshes. Copy it to
`~/.config/omarchy/bar/modules/openusage.qml`, then add `{ "id": "openusage", "type": "qml" }` to a
section of `bar.layout` in `~/.config/omarchy/shell.json`. The bar reloads on save.

## Hyprland

[contrib/hyprland/openusage.lua](../contrib/hyprland/openusage.lua) (Hyprland 0.55 and later, Lua config)
or [openusage.conf](../contrib/hyprland/openusage.conf) (earlier versions) floats a terminal whose title
is `openusage`, for sign-in. Launch one with that title, for example
`xdg-terminal-exec --title=openusage openusage add claude`.

## Commands

| Command | What it does |
| --- | --- |
| `openusage` | Limits JSON for every account. Same output and exit codes as the [Mac command](cli.md): 0 printed, 2 bad arguments or no such account, 4 nothing fetched yet. |
| `openusage <provider>` | Limits JSON for that provider. |
| `openusage <account-id>` | Limits JSON for one account. The id is the one in the JSON (`codex@1a2b3c4d`). |
| `openusage accounts` | List accounts and their ids, in dashboard order. A mirrored CLIProxyAPI account includes its hub host. |
| `openusage hub list` | List connected CLIProxyAPI hubs. |
| `openusage hub add <url>` | Connect a hub. The management key is read with echo off. |
| `openusage hub remove <hub-id-or-host>` | Remove a hub, its key, and its mirrored accounts. |
| `openusage add <provider> [--org <name>]` | Interactive sign-in. |
| `openusage remove <account-id>` | Remove an account and its keyring item. |
| `openusage rename <account-id> <name>` | Rename an account. |
| `openusage refresh` | Refresh every account once. |
| `openusage waybar [--once] [--icon <text>]` | Print Waybar JSON. Without `--once`, keep running. |
| `openusage waybar --refresh` | Ask the running module to refresh every account now. Exits 1 when it is not running. |
| `openusage alerts` | List the quota alert switches. |
| `openusage alerts on\|off [under-10\|under-30\|running-out …]` | Turn alerts on or off; all of them when none is named. |
| `openusage panel` | Open or close the panel (`openusage-panel --toggle`). |

`openusage` on its own reads the last saved snapshots. It does not refresh, the same as on the Mac.
`--force` is accepted and does nothing, as on the Mac.

## Where data lives

| Data | Path |
| --- | --- |
| Accounts and settings | `$XDG_CONFIG_HOME/openusage/settings.plist` (`~/.config/openusage/settings.plist` when unset) |
| Snapshot cache | `$XDG_STATE_HOME/openusage/snapshots.json` (`~/.local/state/openusage/snapshots.json` when unset) |
| Refresh status | `$XDG_STATE_HOME/openusage/status.json` |
| Sign-ins | The desktop keyring, through `secret-tool`. Service `com.robinebers.openusage.accounts`. |
| Daemon pid | `$XDG_RUNTIME_DIR/openusage/daemon.pid` |

`settings.plist` and the keyring service are the same as the previous Linux build, so accounts carry
over. The dashboard layout is the `openusage.layout.v2` entry in that file: the same JSON the Mac app
stores for account order, metric order, and hidden metrics. Linux reads and writes that entry and
leaves every `openusage.layout.v1` key unchanged. It does not copy a 0.7.x layout onto a new account;
the Mac app still does that when it opens. Snapshots are `snapshots.json`. An older `snapshots.plist`
is not read; run `openusage refresh` once to fill the new cache.

`status.json` is one object per account id. A missing id has no backoff. A successful refresh removes
the entry. `retryAt` is UTC with milliseconds.

```json
{
  "claude@8ad3b217": {
    "failures": 1,
    "message": "Claude sign-in expired. Sign in again with `openusage add claude` to keep reading usage.",
    "providerAsked": false,
    "retryAt": "2026-07-13T01:45:00.000Z"
  }
}
```

`providerAsked` is true when the provider asked OpenUsage to wait. The panel and the bar use this file
for the account's error line.

OpenUsage does not read other tools' credentials, keyrings, or auth files. You add each account
yourself. The one thing it reads from another tool is Claude Code's session logs, for Claude's spend
(see Panel).

## Refresh and notifications

`openusage waybar` refreshes every account every 5 minutes, the same interval as the Mac app, and
reprints its line every 30 seconds so reset countdowns move between fetches. At launch
an account read within the last interval keeps its saved reading. `add`, `remove`, and `rename` tell
the running module to reload its accounts (`SIGUSR2`), and it fetches only what is due. A right click
or `openusage waybar --refresh` refreshes every account now. `openusage refresh` does the same fetch
once and prints a line per account; it does not signal the module. A failed fetch keeps the last good
snapshot and backs off, from 5 minutes up to an hour. A rate limit waits until the time the provider
asked for, including when a refresh is forced. `class` includes `stale` once a snapshot is older than
10 minutes. See [Refreshing](refreshing.md).

Quota alerts go out with `notify-send`. The three triggers match the Mac app: under 10% left
(`under-10`), under 30% left (`under-30`), and a limit that may run out before it resets
(`running-out`). All start off. Each alert is sent once per metric per reset window, and only by the
running Waybar module. A metric hidden in the layout never sends one. See [Notifications](settings.md#notifications).

## Proxy

Provider requests use `~/.openusage/config.json`, the same file as on the Mac. `socks5://` and
`http://` are applied by the HTTP client. `https://` proxies are not. Environment proxy variables are
not read. A parse problem is logged without the user name or the password. The browser sign-in page
follows the browser's own proxy. The format is in [Proxy](proxy.md).

## Troubleshooting

Log lines go to stderr. A Waybar session usually hands them to the journal; a copy started in a
terminal prints them itself:

```sh
journalctl --user -u waybar -f
OPENUSAGE_VERBOSE=1 openusage waybar
```

A line looks like `OpenUsage notice: [refresh] …`. Notices, warnings, and errors always print.
`OPENUSAGE_VERBOSE=1` adds info and debug lines. Credentials and response bodies are not logged.

Some compositors leave layer surfaces out of screenshots. To capture the panel for a bug report, run
`OPENUSAGE_DUMP=/tmp/panel.png openusage-panel`. It saves what it draws as a PNG shortly after opening,
then quits. `OPENUSAGE_DUMP_HEIGHT` caps the capture's height so it scrolls as on a screen. `OPENUSAGE_TAB=all` or `priority` picks the tab, and `OPENUSAGE_COLOR_SCHEME=light` or
`dark` forces the appearance. `OPENUSAGE_SCREEN` opens one screen for that capture: `accounts`,
`detail`, `add`, `settings`, `copilot`, `browser`, `device`, `secret`, `hub`, `resets`, `breakdown`,
or `menu`.

A locked keyring makes `secret-tool` fail, and the account reports that OpenUsage could not use the
keyring. Unlock it and run `openusage refresh`. This reads one item back, without printing it into
the command line:

```sh
secret-tool lookup service com.robinebers.openusage.accounts account <account-uuid>
```

Right after login the keyring can answer before it has loaded its items. A lookup that finds nothing
is asked again for a few seconds before the account asks to sign in again, so a reboot doesn't sign
anyone out. Token renewals take a per-account lock in `$XDG_RUNTIME_DIR/openusage`, so the Waybar
module, `openusage refresh`, and the panel never spend the same refresh token twice.

The account id in the keyring is the account UUID in uppercase. If `secret-tool` is not installed, the
error names libsecret and a Secret Service keyring. If `notify-send` is not installed, quota alerts are
skipped and a warning is logged. If `xdg-open` is not installed, a browser sign-in cannot open its
link. If `gtk4-layer-shell` is not installed, the panel does not start.
