import Foundation

/// `openusage alerts`: the Linux stand-in for the Mac Settings screen's notification switches. They are
/// the same `PaceMilestone` settings, off until turned on, and the Waybar module reads them on each pass.
enum LinuxAlerts {
    /// Short command-line names for the switches, in the order the Mac Settings screen lists them.
    static let names: [(name: String, milestone: PaceMilestone)] = [
        ("under-10", .underTenPercent),
        ("under-30", .underThirtyPercent),
        ("running-out", .closeToRunningOut)
    ]

    static func milestones(named requested: [String]) throws -> [PaceMilestone] {
        guard !requested.isEmpty else { return names.map(\.milestone) }
        return try requested.map { name in
            guard let match = names.first(where: { $0.name == name.lowercased() }) else {
                throw LinuxUsageError(message: "Unknown alert: \(name). Expected \(names.map(\.name).joined(separator: ", ")).")
            }
            return match.milestone
        }
    }

    static func list(_ defaults: UserDefaults) -> [String] {
        names.map { entry in
            let state = defaults.bool(forKey: entry.milestone.settingKey) ? "on" : "off"
            return "\(entry.name)\t\(state)\t\(entry.milestone.settingSubtitle)"
        }
    }

    static func set(_ milestones: [PaceMilestone], on: Bool, in defaults: UserDefaults) {
        for milestone in milestones { defaults.set(on, forKey: milestone.settingKey) }
    }
}
