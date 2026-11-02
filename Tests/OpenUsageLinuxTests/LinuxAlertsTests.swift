import XCTest
@testable import OpenUsage

final class LinuxAlertsTests: XCTestCase {
    func testAlertsWithoutArgumentsLists() throws {
        guard case .alerts(nil) = try LinuxCommandLine.parse(["alerts"]) else { return XCTFail("expected a listing") }
    }

    func testOnWithoutNamesTurnsOnEverySwitch() throws {
        guard case .alerts(let change?) = try LinuxCommandLine.parse(["alerts", "on"]) else { return XCTFail("expected a change") }
        XCTAssertTrue(change.on)
        XCTAssertEqual(change.milestones, [.underTenPercent, .underThirtyPercent, .closeToRunningOut])
    }

    func testOffNamesOneSwitch() throws {
        guard case .alerts(let change?) = try LinuxCommandLine.parse(["alerts", "off", "under-30"]) else {
            return XCTFail("expected a change")
        }
        XCTAssertFalse(change.on)
        XCTAssertEqual(change.milestones, [.underThirtyPercent])
    }

    func testUnknownNameAndVerbAreRejected() {
        XCTAssertThrowsError(try LinuxCommandLine.parse(["alerts", "on", "nope"]))
        XCTAssertThrowsError(try LinuxCommandLine.parse(["alerts", "maybe"]))
    }

    /// The switches are the Mac app's settings keys, so the daemon's quota evaluator reads what this writes.
    func testSetWritesTheSharedSettingKeysAndStartsOff() throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent("openusage-alerts-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: directory) }
        let defaults = FileBackedUserDefaults(
            settingsFile: directory.appendingPathComponent("settings.plist"),
            snapshotsFile: directory.appendingPathComponent("snapshots.plist")
        )
        XCTAssertTrue(LinuxAlerts.list(defaults).allSatisfy { $0.contains("\toff\t") })

        LinuxAlerts.set([.closeToRunningOut], on: true, in: defaults)

        XCTAssertTrue(defaults.bool(forKey: "openusage.notifications.closeToRunningOut"))
        XCTAssertFalse(defaults.bool(forKey: "openusage.notifications.underTenPercent"))
        XCTAssertEqual(LinuxAlerts.list(defaults).last?.hasPrefix("running-out\ton\t"), true)
    }
}
