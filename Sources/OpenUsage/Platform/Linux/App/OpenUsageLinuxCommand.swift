import Foundation
#if canImport(Glibc)
import Glibc
#endif

public enum OpenUsageLinuxCommand {
    @MainActor
    public static func main(_ arguments: [String]) async -> Int32 {
        AppLog.bootstrap()
        do {
            switch try LinuxCommandLine.parse(arguments) {
            case .help:
                print(help)
                return 0
            case .read(let providerID, let force):
                return try read(providerID: providerID, force: force)
            case .accounts:
                return accounts()
            case .add(let provider, let organization):
                return await add(provider: provider, organization: organization)
            case .remove(let accountID):
                return remove(accountID)
            case .rename(let accountID, let name):
                return rename(accountID, to: name)
            case .refresh:
                return await refresh()
            case .waybar(let once, let icon):
                return await WaybarDaemon.run(once: once, icon: icon)
            case .waybarRefresh:
                guard WaybarDaemon.signalRunning(SIGUSR1) else {
                    LinuxText.error("The Waybar module is not running.")
                    return 1
                }
                return 0
            case .alerts(let change):
                return alerts(change)
            }
        } catch let error as LinuxUsageError {
            LinuxText.error("\(error.message)\nRun 'openusage --help' for usage.")
            return 2
        } catch {
            LinuxText.error(error.localizedDescription)
            return 1
        }
    }

    @MainActor
    private static func read(providerID: String?, force: Bool) throws -> Int32 {
        if force {
            FileHandle.standardError.write(Data(
                "openusage: --force does nothing; the command prints what the app last fetched\n".utf8
            ))
        }
        do {
            let result = try UsageReader.read(FileBackedUserDefaults.openUsage(), providerID: providerID)
            FileHandle.standardOutput.write(result.data)
            FileHandle.standardOutput.write(Data("\n".utf8))
            guard result.hasSnapshots else {
                LinuxText.error("nothing fetched yet; add an account with 'openusage add <provider>' or run 'openusage refresh'")
                return 4
            }
            return 0
        } catch UsageReaderError.unknownProvider(let providerID) {
            LinuxText.error("Unknown provider: \(providerID)")
            return 2
        }
    }

    @MainActor
    private static func accounts() -> Int32 {
        let session = LinuxSession()
        let providers = session.layout.orderedProviders()
        guard !providers.isEmpty else {
            LinuxText.line("No accounts.")
            return 0
        }
        for provider in providers {
            var fields = [provider.id, provider.displayName, provider.accountLabel ?? provider.displayName]
            if let plan = session.data.plan(for: provider.id), !plan.isEmpty { fields.append(plan) }
            LinuxText.line(fields.joined(separator: "\t"))
        }
        return 0
    }

    @MainActor
    private static func add(provider: String, organization: String?) async -> Int32 {
        guard let kind = ProviderKind(rawValue: provider.lowercased()) else {
            LinuxText.error("Unknown provider: \(provider). Expected \(providerNames).")
            return 2
        }
        let organization = organization?.trimmingCharacters(in: .whitespacesAndNewlines)
        if let organization {
            guard kind == .copilot else {
                LinuxText.error("--org is only for a Copilot organization account.")
                return 2
            }
            guard !organization.isEmpty else {
                LinuxText.error("--org needs an organization name.")
                return 2
            }
            return await LinuxSignIn.add(kind: kind, organization: organization)
        }
        return await LinuxSignIn.add(kind: kind, organization: nil)
    }

    @MainActor
    private static func remove(_ accountID: String) -> Int32 {
        let store = AccountStore(defaults: FileBackedUserDefaults.openUsage())
        guard let account = account(accountID, in: store) else {
            LinuxText.error("No account \(accountID).")
            return 2
        }
        store.remove(account.id)
        guard !store.accounts.contains(where: { $0.id == account.id }) else {
            LinuxText.error(store.lastError ?? "The account could not be removed.")
            return 1
        }
        WaybarDaemon.notifyRunning()
        LinuxText.line("Removed \(account.cardID).")
        return 0
    }

    @MainActor
    private static func rename(_ accountID: String, to name: String) -> Int32 {
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            LinuxText.error("rename needs a name.")
            return 2
        }
        let store = AccountStore(defaults: FileBackedUserDefaults.openUsage())
        guard let account = account(accountID, in: store) else {
            LinuxText.error("No account \(accountID).")
            return 2
        }
        store.rename(account.id, to: trimmed)
        WaybarDaemon.notifyRunning()
        LinuxText.line("Renamed \(account.cardID) to \(trimmed).")
        return 0
    }

    @MainActor
    private static func refresh() async -> Int32 {
        let session = LinuxSession()
        let providers = session.layout.orderedProviders()
        guard !providers.isEmpty else {
            LinuxText.line("No accounts.")
            return 0
        }
        let results = await session.refresh(force: true)
        var failed = false
        for result in results {
            if result.outcome == .failed { failed = true }
            LinuxText.line(
                "\(result.provider.id) (\(result.provider.fullName)): \(session.describe(result.outcome, providerID: result.provider.id))"
            )
        }
        return failed ? 1 : 0
    }

    private static func alerts(_ change: (on: Bool, milestones: [PaceMilestone])?) -> Int32 {
        let defaults = FileBackedUserDefaults.openUsage()
        if let change { LinuxAlerts.set(change.milestones, on: change.on, in: defaults) }
        for line in LinuxAlerts.list(defaults) { LinuxText.line(line) }
        return 0
    }

    @MainActor
    private static func account(_ token: String, in store: AccountStore) -> Account? {
        let token = token.lowercased()
        return store.accounts.first { $0.cardID == token }
    }

    private static var providerNames: String {
        ProviderKind.allCases.map(\.rawValue).joined(separator: ", ")
    }

    private static var help: String {
        """
        Usage: openusage [--force] [provider|account-id]
               openusage accounts
               openusage add <provider> [--org <name>]
               openusage remove <account-id>
               openusage rename <account-id> <name>
               openusage refresh
               openusage waybar [--once] [--icon <text>]
               openusage waybar --refresh
               openusage alerts [on|off [under-10|under-30|running-out ...]]

        With no command, print the usage last fetched as openusage.limits.v2 JSON.

          provider     An account id (codex@1a2b3c4d) or a provider kind
                       (\(providerNames)).

        Commands:
          accounts     List accounts: id, provider, label, and plan when one is cached.
          add          Sign in. Browser sign-in opens the URL and prints it. API keys and
                       tokens are read from stdin with echo off in a terminal. --org adds a
                       Copilot organization by its token.
          remove       Remove an account and its saved sign-in.
          rename       Rename an account.
          refresh      Fetch every account now.
          waybar       Print Waybar JSON lines. Refreshes on start and every 5 minutes.
                       --once prints the cache and exits. --icon is the text used when
                       nothing is running out (default: a gauge glyph). A click on the
                       module runs --refresh, which asks the running module to refresh every
                       account now (SIGUSR1).
          alerts       List the desktop notification switches, or turn them on or off
                       (all of them when none is named). All start off.

        Files:
          \(OpenUsagePaths.settingsFile.path)
          \(OpenUsagePaths.snapshotsFile.path)
          \(OpenUsagePaths.waybarPID.path)

        Options:
          --force      Accepted when printing usage, then ignored.
          -h, --help   Show this help.

        Exit codes: 0 printed or completed, 2 bad arguments or an unknown provider,
        4 nothing fetched yet. A failed refresh exits 1. Ctrl-C during sign-in exits 130.
        """
    }
}

struct LinuxUsageError: Error {
    var message: String
}

enum LinuxCommandLine {
    static func parse(_ arguments: [String]) throws -> LinuxCommand {
        if arguments.contains(where: { $0 == "-h" || $0 == "--help" }) { return .help }

        var index = 0
        var force = false
        var once = false
        var organization: String?
        var icon: String?
        var waybarRefresh = false
        var positionals: [String] = []
        while index < arguments.count {
            let argument = arguments[index]
            switch argument {
            case "--force":
                force = true
            case "--once":
                once = true
            case "--refresh":
                waybarRefresh = true
            case "--org":
                index += 1
                guard index < arguments.count, !arguments[index].hasPrefix("-") else {
                    throw LinuxUsageError(message: "--org needs an organization name.")
                }
                organization = arguments[index]
            case "--icon":
                index += 1
                guard index < arguments.count, !arguments[index].isEmpty else {
                    throw LinuxUsageError(message: "--icon needs a value.")
                }
                icon = arguments[index]
            case let option where option.hasPrefix("-"):
                throw LinuxUsageError(message: "Unknown option: \(option)")
            default:
                positionals.append(argument)
            }
            index += 1
        }

        if waybarRefresh, positionals.first != "waybar" {
            throw LinuxUsageError(message: "--refresh only applies to waybar.")
        }
        guard let command = positionals.first else {
            try unexpected(force: force, once: once, organization: organization, icon: icon, allowForce: true)
            return .read(providerID: nil, force: force)
        }
        switch command {
        case "help":
            return .help
        case "accounts":
            guard positionals.count == 1 else { throw LinuxUsageError(message: "accounts takes no arguments.") }
            try unexpected(force: force, once: once, organization: organization, icon: icon)
            return .accounts
        case "add":
            guard positionals.count == 2 else { throw LinuxUsageError(message: "add needs a provider.") }
            try unexpected(force: force, once: once, organization: organization, icon: icon, allowOrg: true)
            return .add(provider: positionals[1], organization: organization)
        case "remove":
            guard positionals.count == 2 else { throw LinuxUsageError(message: "remove needs an account id.") }
            try unexpected(force: force, once: once, organization: organization, icon: icon)
            return .remove(accountID: positionals[1])
        case "rename":
            guard positionals.count >= 3 else { throw LinuxUsageError(message: "rename needs an account id and a name.") }
            try unexpected(force: force, once: once, organization: organization, icon: icon)
            return .rename(accountID: positionals[1], name: positionals.dropFirst(2).joined(separator: " "))
        case "refresh":
            guard positionals.count == 1 else { throw LinuxUsageError(message: "refresh takes no arguments.") }
            try unexpected(force: force, once: once, organization: organization, icon: icon)
            return .refresh
        case "alerts":
            try unexpected(force: force, once: once, organization: organization, icon: icon)
            guard positionals.count > 1 else { return .alerts(nil) }
            guard positionals[1] == "on" || positionals[1] == "off" else {
                throw LinuxUsageError(message: "alerts takes on or off, then optional alert names.")
            }
            let milestones = try LinuxAlerts.milestones(named: Array(positionals.dropFirst(2)))
            return .alerts((on: positionals[1] == "on", milestones: milestones))
        case "waybar":
            guard positionals.count == 1 else { throw LinuxUsageError(message: "waybar takes no arguments.") }
            if waybarRefresh {
                guard !once, icon == nil else { throw LinuxUsageError(message: "--refresh takes no other options.") }
                try unexpected(force: force, once: false, organization: organization, icon: nil)
                return .waybarRefresh
            }
            try unexpected(force: force, once: once, organization: organization, icon: icon, allowOnce: true, allowIcon: true)
            return .waybar(once: once, icon: icon ?? WaybarOutput.defaultIcon)
        default:
            guard positionals.count == 1 else { throw LinuxUsageError(message: "Only one provider can be requested at a time.") }
            try unexpected(force: force, once: once, organization: organization, icon: icon, allowForce: true)
            return .read(providerID: command, force: force)
        }
    }

    private static func unexpected(
        force: Bool, once: Bool, organization: String?, icon: String?,
        allowForce: Bool = false, allowOnce: Bool = false, allowOrg: Bool = false, allowIcon: Bool = false
    ) throws {
        if force, !allowForce { throw LinuxUsageError(message: "--force only applies when printing usage.") }
        if once, !allowOnce { throw LinuxUsageError(message: "--once only applies to waybar.") }
        if organization != nil, !allowOrg { throw LinuxUsageError(message: "--org only applies to add.") }
        if icon != nil, !allowIcon { throw LinuxUsageError(message: "--icon only applies to waybar.") }
    }
}

enum LinuxCommand {
    case help
    case read(providerID: String?, force: Bool)
    case accounts
    case add(provider: String, organization: String?)
    case remove(accountID: String)
    case rename(accountID: String, name: String)
    case refresh
    case waybar(once: Bool, icon: String)
    case waybarRefresh
    case alerts((on: Bool, milestones: [PaceMilestone])?)
}
