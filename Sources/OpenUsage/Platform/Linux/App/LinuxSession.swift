import Foundation

@MainActor
final class LinuxSession {
    let defaults: UserDefaults
    private let http: any HTTPClient
    private let vault: any CredentialVault
    private let hubGate: CLIProxyHubGate
    private let claudeSessions: ClaudeSessions
    private(set) var accounts: AccountStore
    private(set) var providers: [AccountProviderRuntime]
    private(set) var layout: LayoutStore
    private(set) var data: WidgetDataStore
    private(set) var hubs: CLIProxyHubSync

    init(
        defaults: UserDefaults = FileBackedUserDefaults.openUsage(),
        http: any HTTPClient = URLSessionHTTPClient(),
        vault: any CredentialVault = KeychainCredentialVault()
    ) {
        self.defaults = defaults
        self.http = http
        self.vault = vault
        let hubGate = CLIProxyHubGate()
        self.hubGate = hubGate
        let claudeSessions = ClaudeSessions(defaults: defaults)
        self.claudeSessions = claudeSessions
        let accounts = AccountStore(defaults: defaults, vault: vault)
        self.accounts = accounts
        let providers = ProviderCatalog.make(
            accounts: accounts.accounts, vault: vault, http: http, hubGate: hubGate, claudeSessions: claudeSessions
        )
        self.providers = providers
        let registry = WidgetRegistry.from(providers)
        layout = LayoutStore(registry: registry, defaults: defaults)
        data = WidgetDataStore(
            registry: registry, providers: providers, cache: ProviderSnapshotCache(userDefaults: defaults), defaults: defaults
        )
        hubs = CLIProxyHubSync(accounts: accounts, client: CLIProxyHubClient(http: http, gate: hubGate))
        bindAccountChanges()
    }

    func accountsChangedOnDisk() -> Bool {
        let fresh = AccountStore(defaults: defaults, vault: vault)
        return Self.token(fresh) != Self.token(accounts)
    }

    /// Picks up accounts and cached usage written by another process, and asks a reused runtime to
    /// drop a refresh that finished against an account that has since been removed.
    func reloadFromDisk() {
        let fresh = AccountStore(defaults: defaults, vault: vault)
        accounts.didChange = nil
        accounts = fresh
        bindAccountChanges()
        providers = ProviderCatalog.make(
            accounts: accounts.accounts, vault: vault, http: http, hubGate: hubGate, claudeSessions: claudeSessions,
            reusing: providers
        )
        let registry = WidgetRegistry.from(providers)
        data.snapshots = ProviderSnapshotCache(userDefaults: defaults).load()
        layout.apply(registry: registry)
        data.apply(providers: providers, registry: registry)
        for runtime in providers {
            if let snapshot = data.snapshots[runtime.provider.id] { runtime.restore(snapshot) }
        }
        hubs = CLIProxyHubSync(accounts: accounts, client: CLIProxyHubClient(http: http, gate: hubGate))
    }

    func refresh(force: Bool) async -> [(provider: Provider, outcome: WidgetDataStore.RefreshOutcome)] {
        let ordered = layout.orderedProviders()
        let outcomes = await data.refresh(providerIDs: ordered.map(\.id), force: force)
        return Array(zip(ordered, outcomes)).map { (provider: $0.0, outcome: $0.1) }
    }

    func describe(_ outcome: WidgetDataStore.RefreshOutcome, providerID: String) -> String {
        switch outcome {
        case .refreshed: "updated"
        case .cacheHit: "cached"
        case .skipped: "skipped"
        case .failed, .backedOff: data.problem(for: providerID) ?? "failed"
        }
    }

    /// Quota alerts use the same switches as the Mac app, and those switches start off.
    func evaluateQuotas(_ evaluator: QuotaNotificationEvaluator, now: Date = Date()) async {
        let layout = layout
        let defaults = defaults
        await evaluator.evaluate(
            layout.visibleDescriptors,
            data: data.data(for:),
            isOn: { defaults.bool(forKey: $0.settingKey) },
            now: now,
            providerName: { layout.provider(id: $0)?.fullName ?? $0 }
        )
    }

    func viewModel(icon: String, now: Date = Date()) -> WaybarViewModel {
        let providers = layout.orderedProviders()
        let dashboard = data.accounts(in: layout.displayGroups)
        let board = PriorityBoard.make(accounts: dashboard, now: now)
        let content = MenuBarContentBuilder.build(board: board, limit: data.menuBarLimit)
        let byID = Dictionary(uniqueKeysWithValues: dashboard.map { ($0.id, $0) })
        var blocks: [WaybarAccount] = []
        var worst = WaybarSeverity.normal
        var percentage = 0
        var failed = false
        var stale = false
        for provider in providers {
            if data.errorMessage(for: provider.id) != nil { failed = true }
            if data.snapshots[provider.id]?.isStale(at: now) == true { stale = true }
            var metrics: [WaybarMetric] = []
            for line in byID[provider.id]?.lines ?? [] {
                let widget = line.data
                var fraction: Double?
                if widget.hasData, let limit = widget.limit, limit > 0 {
                    percentage = max(percentage, WaybarOutput.usedPercent(used: widget.used, limit: limit))
                    fraction = widget.fraction
                    worst = max(worst, WaybarSeverity(widget.severity))
                }
                metrics.append(WaybarMetric(
                    title: widget.title, fraction: fraction, value: widget.valueText,
                    reset: widget.compactResetText(now: now)
                ))
            }
            blocks.append(WaybarAccount(
                title: provider.fullName, plan: data.plan(for: provider.id),
                problem: data.problem(for: provider.id), metrics: metrics
            ))
        }
        return WaybarViewModel(
            content: content, icon: icon, accounts: blocks, hasAccounts: !providers.isEmpty,
            failed: failed, stale: stale, percentage: percentage, severity: worst
        )
    }

    func waybarLine(icon: String, now: Date = Date()) -> String {
        WaybarOutput.json(viewModel(icon: icon, now: now))
    }

    private func bindAccountChanges() {
        accounts.didChange = { [weak self] in self?.syncProviders() }
    }

    private func syncProviders() {
        providers = ProviderCatalog.make(
            accounts: accounts.accounts, vault: vault, http: http, hubGate: hubGate, claudeSessions: claudeSessions,
            reusing: providers
        )
        let registry = WidgetRegistry.from(providers)
        layout.apply(registry: registry)
        data.apply(providers: providers, registry: registry)
    }

    private static func token(_ store: AccountStore) -> String {
        let accounts = store.accounts.map {
            "\($0.id.uuidString)|\($0.label)|\($0.organization ?? "")|\($0.hub?.loginID ?? "")|\($0.hub?.authIndex ?? "")"
        }
        let hubs = store.hubs.map { "\($0.id.uuidString)|\($0.url.absoluteString)" }
        return (accounts + ["--"] + hubs).joined(separator: "\n")
    }
}
