# Linux

OpenUsage on Linux is the same providers and sign-ins as the Mac app, with no window. A long-running
`openusage waybar` process feeds a Waybar module, and the `openusage` command lists accounts, signs
in, refreshes, and prints limits.

## Requirements

- Swift 6.2, to build. The install script links the Swift standard library into the binary, so a
  Swift runtime is not needed afterwards.
- `secret-tool` (libsecret), and a Secret Service keyring such as GNOME Keyring, or KWallet with
  Secret Service turned on.
- `notify-send` (libnotify), for quota alerts.
- `xdg-open`, so a browser sign-in can open its link.
- A Nerd Font, for the default Waybar icon. Pass `--icon` to use other text.

## Install

```sh
script/install_linux.sh
```

The script builds with `swift build -c release --static-swift-stdlib --disable-automatic-resolution`,
using `swift` on `PATH` when it is Swift 6.2, otherwise `mise exec swift@6.2 --`. It installs the
binary to `~/.local/bin/openusage` and warns if `secret-tool`, `notify-send`, or `xdg-open` is missing.

`~/.local/bin` has to be on `PATH`.

A plain `swift build` or `swift test` on Linux deletes `Package.resolved`. The Linux manifest has no
package dependencies, so SwiftPM drops the Mac app's pins. Pass `--disable-automatic-resolution` on
every Linux `swift build` and `swift test`. Never commit a deleted `Package.resolved`.

## Add an account

```sh
openusage add claude
openusage add copilot --org my-org
```

Sign-in runs in the terminal. A browser sign-in opens the link with `xdg-open`. A device-code sign-in
prints the code to enter. An API-key sign-in reads the key from stdin. The provider ids match the Mac
app: `claude`, `codex`, `cursor`, `antigravity`, `copilot`, `devin`, `grok`, `ollama`, `opencode`,
`openrouter`, `zai`. What each sign-in grants is in [Providers](providers.md).

`--org <name>` adds a Copilot organization's billing as its own account, signed in with an organization
token (`openusage add copilot --org <name>`).

## Waybar

Merge [contrib/waybar/config.jsonc](../contrib/waybar/config.jsonc) into the Waybar config, and add
`custom/openusage` to a modules list. Copy the classes in
[contrib/waybar/style.css](../contrib/waybar/style.css) into the Waybar stylesheet.

`openusage waybar` stays running and prints one JSON object per line (`return-type: json`) on each
update. It refreshes every 5 minutes. `SIGUSR1` refreshes every account now; the sample module's left
click sends that signal. Right-click opens a terminal on `openusage accounts`, using `xdg-terminal-exec` when it
is installed.

`--once` prints one line from the saved snapshots and exits, without refreshing. When no limit is running
low, the module shows only an icon, as the Mac menu bar shows only the app icon; `--icon <text>` replaces
that icon (a Nerd Font gauge by default).

| Field | Meaning |
| --- | --- |
| `text` | The same strip as the Mac menu bar: the limits closest to running out, or the icon when none is. |
| `tooltip` | Pango markup of every account. |
| `class` | `normal`, `warning`, or `critical`, plus `error`, `empty`, or `stale` when that is the state. |
| `percentage` | The module percentage Waybar reads for `{percentage}`. |
| `alt` | Alternate text for `{alt}`. |

## Hyprland

[contrib/hyprland/openusage.conf](../contrib/hyprland/openusage.conf) floats a terminal whose title is
`openusage`, for `openusage add` and the Waybar right-click. Launch a sign-in with that title, for
example `kitty --title openusage -e openusage add claude`.

## Commands

| Command | What it does |
| --- | --- |
| `openusage` | Limits JSON for every account. Same output and exit codes as the [Mac command](cli.md): 0 printed, 2 bad arguments or no such account, 4 nothing fetched yet. |
| `openusage <provider>` | Limits JSON for that provider. |
| `openusage <account-id>` | Limits JSON for one account. The id is the one in the JSON (`codex@1a2b3c4d`). |
| `openusage accounts` | List accounts and their ids. |
| `openusage add <provider> [--org <name>]` | Interactive sign-in. |
| `openusage remove <account-id>` | Remove an account and its keyring item. |
| `openusage rename <account-id> <name>` | Rename an account. |
| `openusage refresh` | Refresh every account once. |
| `openusage waybar [--once] [--icon <text>]` | Print Waybar JSON. Without `--once`, keep running. |
| `openusage alerts` | List the quota alert switches. |
| `openusage alerts on\|off [under-10\|under-30\|running-out …]` | Turn alerts on or off; all of them when none is named. |

`openusage` on its own reads the last saved snapshots. It does not refresh, the same as on the Mac.
`--force` is accepted and does nothing, as on the Mac.

## Where data lives

| Data | Path |
| --- | --- |
| Accounts and settings | `$XDG_CONFIG_HOME/openusage/settings.plist` (`~/.config/openusage/settings.plist` when unset) |
| Snapshot cache | `$XDG_STATE_HOME/openusage/snapshots.plist` (`~/.local/state/openusage/snapshots.plist` when unset) |
| Sign-ins | The desktop keyring, through `secret-tool`. Service `com.robinebers.openusage.accounts`. |
| Waybar pid | `$XDG_RUNTIME_DIR/openusage/waybar.pid` |

OpenUsage does not read other tools' files, keyrings, or environment variables for credentials. You add
each account yourself.

## Refresh and notifications

`openusage waybar` refreshes every account every 5 minutes, the same interval as the Mac app. At launch
(Waybar restarts it on every config reload) an account read within the last interval keeps its saved
reading. `add`, `remove`, and `rename` tell the running module to reload its accounts, and it fetches
only the account that is new. A left click, `SIGUSR1`, or `openusage refresh` refreshes every account
once, ahead of that interval. A failed
fetch keeps the last good snapshot. `class` includes `stale` once a snapshot is older than two
intervals (10 minutes): the same moment the Mac app shows a clock and the limits JSON sets
`"stale": true`. See [Refreshing](refreshing.md).

Quota alerts go out with `notify-send` to the notification daemon (mako, dunst, swaync, or another).
The three triggers match the Mac app: under 10% left (`under-10`), under 30% left (`under-30`), and a
limit that may run out before it resets (`running-out`). All start off, as on the Mac; turn them on with
`openusage alerts on`. Each alert is sent once per metric per reset window. See
[Notifications](settings.md#notifications).

## Proxy

Provider requests use `~/.openusage/config.json`, the same file as on the Mac. The format is in
[Proxy](proxy.md).

On Linux the proxy is applied inside this process for libcurl, which carries Foundation's HTTP:
`all_proxy`, `ALL_PROXY`, `http_proxy`, `HTTP_PROXY`, `https_proxy`, and `HTTPS_PROXY`. A `socks5://`
URL is sent as `socks5h://` so the proxy resolves host names. With no file, `"enabled": false`, or a
file that does not parse, OpenUsage sets no proxy. A parse problem is logged on stderr without the
user name or the password.

The browser sign-in page follows the browser's own proxy. The sign-in callback stays on localhost.

## Troubleshooting

Log lines go to stderr. A Waybar session usually hands them to the journal; a copy started in a
terminal prints them itself:

```sh
journalctl --user -u waybar -f
openusage waybar
```

A line looks like `OpenUsage notice: [refresh] …`. Credentials and response bodies are not logged.

A locked keyring makes `secret-tool` fail, and the account reports that OpenUsage could not use the
keyring. Unlock it (log in, or unlock GNOME Keyring) and run `openusage refresh`. This reads one item
back, without printing it into the command line:

```sh
secret-tool lookup service com.robinebers.openusage.accounts account <account-uuid>
```

If `secret-tool` is not installed, the error names libsecret and a Secret Service keyring. If
`notify-send` is not installed, quota alerts are skipped and a warning is logged. If `xdg-open` is not
installed, a browser sign-in cannot open its link.
