import XCTest
@testable import OpenUsage

final class WaybarOutputTests: XCTestCase {
    private let claude = Provider(id: "claude@11111111", kind: .claude, accountLabel: "Personal")
    private let codex = Provider(id: "codex@22222222", kind: .codex, accountLabel: "Work")

    func testStripUsesProviderShortLabelAndValue() {
        var content = MenuBarContent(items: [
            MenuBarContent.Item(id: "session", provider: claude, label: "Session", shortLabel: "Session", value: "18%"),
            MenuBarContent.Item(id: "weekly", provider: codex, label: "Weekly", shortLabel: "Weekly", value: "4%")
        ])
        content.overflow = 2
        XCTAssertEqual(WaybarOutput.strip(content, icon: "◆"), "Claude Session 18% · ChatGPT Weekly 4% +2")
    }

    func testStripAddsTheAccountBadgeWhenTheProviderHasSeveralAccounts() {
        var provider = claude
        provider.badge = "P"
        let content = MenuBarContent(items: [
            MenuBarContent.Item(id: "session", provider: provider, label: "Session", shortLabel: "Session", value: "18%")
        ])
        XCTAssertEqual(WaybarOutput.strip(content, icon: "◆"), "Claude P Session 18%")
    }

    func testEmptyStripUsesTheIcon() {
        XCTAssertEqual(WaybarOutput.strip(MenuBarContent(items: []), icon: "◆"), "◆")
    }

    @MainActor
    func testStripFollowsTheMenuBarBuilder() {
        var session = WidgetData(title: "Session", kind: .percent, used: 82, limit: 100)
        session.displayMode = .used
        var weekly = WidgetData(title: "Weekly", kind: .percent, used: 96, limit: 100)
        weekly.displayMode = .used
        let content = MenuBarContentBuilder.build(
            board: PriorityBoard.make(accounts: [
                account(claude, session),
                account(codex, weekly)
            ], now: Date(timeIntervalSince1970: 1_800_000_000)),
            limit: .upToThree
        )
        XCTAssertEqual(WaybarOutput.strip(content, icon: "◆"), "ChatGPT Weekly 96% · Claude Session 82%")
    }

    func testBarRoundsToEightCells() {
        XCTAssertEqual(WaybarOutput.bar(0), "░░░░░░░░")
        XCTAssertEqual(WaybarOutput.bar(0.5), "████░░░░")
        XCTAssertEqual(WaybarOutput.bar(1), "████████")
        XCTAssertEqual(WaybarOutput.bar(1.4), "████████")
    }

    func testUsedPercentClamps() {
        XCTAssertEqual(WaybarOutput.usedPercent(used: 18, limit: 100), 18)
        XCTAssertEqual(WaybarOutput.usedPercent(used: 140, limit: 100), 100)
        XCTAssertEqual(WaybarOutput.usedPercent(used: 5, limit: 0), 0)
    }

    func testTooltipEscapesMarkupAndIncludesTheBarValueAndReset() throws {
        let model = WaybarViewModel(
            content: MenuBarContent(items: []),
            icon: "◆",
            accounts: [
                WaybarAccount(
                    title: "Claude · A <B> & C",
                    plan: "Pro",
                    problem: "Last updated 2h ago",
                    metrics: [
                        WaybarMetric(title: "Session", fraction: 0.5, value: "50% left", reset: "4h 12m")
                    ]
                )
            ],
            hasAccounts: true,
            failed: true,
            stale: true,
            percentage: 50,
            severity: .warning
        )
        let payload = try decode(model)
        XCTAssertEqual(payload.text, "◆")
        XCTAssertEqual(payload.classes, ["warning", "error", "stale"])
        XCTAssertEqual(payload.alt, "warning error stale")
        XCTAssertEqual(payload.percentage, 50)
        XCTAssertTrue(payload.tooltip.contains("<b>Claude · A &lt;B&gt; &amp; C</b> · Pro"))
        XCTAssertTrue(payload.tooltip.contains("Session  ████░░░░  50% left  4h 12m"))
        XCTAssertTrue(payload.tooltip.contains("Last updated 2h ago"))
        XCTAssertFalse(WaybarOutput.json(model).contains("\n"))
    }

    func testEmptyAccountsAreTheirOwnClass() throws {
        let model = WaybarViewModel(
            content: MenuBarContent(items: []), icon: "◆", accounts: [], hasAccounts: false,
            failed: false, stale: false, percentage: 0, severity: .normal
        )
        let payload = try decode(model)
        XCTAssertEqual(payload.classes, ["empty"])
        XCTAssertEqual(payload.alt, "empty")
        XCTAssertEqual(payload.text, "◆")
        XCTAssertEqual(payload.percentage, 0)
    }

    func testPathsFollowXDGAndTheHomeDefaults() {
        let environment = ["XDG_CONFIG_HOME": "/tmp/cfg", "XDG_STATE_HOME": "/tmp/state", "XDG_RUNTIME_DIR": "/tmp/run"]
        XCTAssertEqual(
            OpenUsagePaths.configDirectory(environment: environment, home: "/home/jay").path,
            "/tmp/cfg/openusage"
        )
        XCTAssertEqual(
            OpenUsagePaths.stateDirectory(environment: environment, home: "/home/jay").path,
            "/tmp/state/openusage"
        )
        XCTAssertEqual(
            OpenUsagePaths.runtimeDirectory(environment: environment, home: "/home/jay").path,
            "/tmp/run/openusage"
        )
        XCTAssertEqual(
            OpenUsagePaths.configDirectory(environment: [:], home: "/home/jay").path,
            "/home/jay/.config/openusage"
        )
        XCTAssertEqual(
            OpenUsagePaths.stateDirectory(environment: [:], home: "/home/jay").path,
            "/home/jay/.local/state/openusage"
        )
        XCTAssertTrue(OpenUsagePaths.runtimeDirectory(environment: [:], home: "/home/jay").path.hasPrefix("/tmp/openusage-"))
    }

    func testFileStoreIsVisibleToASecondInstance() throws {
        let (first, second, _) = try makeDefaults()
        first.set(Data("accounts".utf8), forKey: "accounts.v1")
        first.set(true, forKey: "openusage.notifications.underTenPercent")
        first.set(MenuBarLimit.upToOne.rawValue, forKey: "menuBarLimit")
        first.set(["claude", "codex"], forKey: "pins")

        XCTAssertEqual(second.data(forKey: "accounts.v1"), Data("accounts".utf8))
        XCTAssertEqual(second.object(forKey: "openusage.notifications.underTenPercent") as? Bool, true)
        XCTAssertTrue(second.bool(forKey: "openusage.notifications.underTenPercent"))
        XCTAssertTrue(second.bool(forKey: "openusage.notifications.underTenPercent", default: false))
        XCTAssertFalse(second.bool(forKey: "missing"))
        XCTAssertTrue(second.bool(forKey: "missing", default: true))
        XCTAssertEqual(second.enumValue(forKey: "menuBarLimit", default: MenuBarLimit.upToThree), .upToOne)
        XCTAssertEqual(second.stringArray(forKey: "pins"), ["claude", "codex"])

        second.set("upToTwo", forKey: "menuBarLimit")
        second.removeObject(forKey: "pins")
        XCTAssertEqual(first.string(forKey: "menuBarLimit"), "upToTwo")
        XCTAssertEqual(first.data(forKey: "accounts.v1"), Data("accounts".utf8))
        XCTAssertNil(first.stringArray(forKey: "pins"))
    }

    func testSnapshotKeyLivesInTheStateFile() throws {
        let (first, second, urls) = try makeDefaults()
        let snapshot = ProviderSnapshot(
            providerID: "claude@abcd1234", displayName: "Claude", lines: [],
            refreshedAt: Date(timeIntervalSince1970: 0)
        )
        ProviderSnapshotCache(userDefaults: first).save(["claude@abcd1234": snapshot])
        first.set(Data("[]".utf8), forKey: "accounts.v1")

        let loaded = ProviderSnapshotCache(userDefaults: second).load()
        XCTAssertEqual(loaded["claude@abcd1234"]?.providerID, "claude@abcd1234")
        XCTAssertEqual(second.data(forKey: "accounts.v1"), Data("[]".utf8))

        let settings = try String(contentsOf: urls.settings, encoding: .utf8)
        let snapshots = try String(contentsOf: urls.snapshots, encoding: .utf8)
        XCTAssertTrue(settings.contains("accounts.v1"))
        XCTAssertFalse(settings.contains("providerSnapshots"))
        XCTAssertTrue(snapshots.contains(OpenUsageStorageKey.snapshots))
        XCTAssertFalse(snapshots.contains("accounts.v1"))
    }

    func testConcurrentWritersDoNotDropEachOthersKeys() async throws {
        let urls = try makeDefaults().2
        let settings = urls.settings
        let snapshots = urls.snapshots
        await withTaskGroup(of: Void.self) { group in
            for index in 0..<24 {
                group.addTask {
                    // A fresh instance per write: the processes share the file, not an object.
                    let writer = FileBackedUserDefaults(settingsFile: settings, snapshotsFile: snapshots)
                    writer.set(Data([UInt8(index)]), forKey: "key-\(index)")
                }
            }
        }
        let reader = FileBackedUserDefaults(settingsFile: urls.settings, snapshotsFile: urls.snapshots)
        for index in 0..<24 {
            XCTAssertEqual(reader.data(forKey: "key-\(index)"), Data([UInt8(index)]), "key-\(index)")
        }
    }

    private func account(_ provider: Provider, _ data: WidgetData) -> DashboardAccount {
        DashboardAccount(provider: provider, lines: [
            DashboardLine(
                descriptor: WidgetDescriptor(provider: provider, key: data.title, metricLabel: data.title, sample: data),
                data: data
            )
        ])
    }

    private struct DecodedWaybar: Decodable {
        var text: String
        var tooltip: String
        var classes: [String]
        var percentage: Int
        var alt: String

        enum CodingKeys: String, CodingKey {
            case text, tooltip, percentage, alt
            case classes = "class"
        }
    }

    private func decode(_ model: WaybarViewModel) throws -> DecodedWaybar {
        let data = try XCTUnwrap(WaybarOutput.json(model).data(using: .utf8))
        return try JSONDecoder().decode(DecodedWaybar.self, from: data)
    }

    private func makeDefaults() throws -> (FileBackedUserDefaults, FileBackedUserDefaults, (settings: URL, snapshots: URL)) {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent("openusage-tests-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        addTeardownBlock { try? FileManager.default.removeItem(at: directory) }
        let settings = directory.appendingPathComponent("settings.plist")
        let snapshots = directory.appendingPathComponent("snapshots.plist")
        return (
            FileBackedUserDefaults(settingsFile: settings, snapshotsFile: snapshots),
            FileBackedUserDefaults(settingsFile: settings, snapshotsFile: snapshots),
            (settings, snapshots)
        )
    }
}
