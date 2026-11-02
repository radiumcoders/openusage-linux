import Foundation

enum WaybarSeverity: String, Comparable {
    case normal, warning, critical

    static func < (lhs: WaybarSeverity, rhs: WaybarSeverity) -> Bool { lhs.rank < rhs.rank }

    init(_ severity: WidgetData.MeterSeverity) {
        switch severity {
        case .normal: self = .normal
        case .warning: self = .warning
        case .critical: self = .critical
        }
    }

    private var rank: Int {
        switch self {
        case .normal: 0
        case .warning: 1
        case .critical: 2
        }
    }
}

struct WaybarMetric: Equatable {
    var title: String
    var fraction: Double?
    var value: String
    var reset: String?
}

struct WaybarAccount: Equatable {
    var title: String
    var plan: String?
    var problem: String?
    var metrics: [WaybarMetric]
}

struct WaybarViewModel {
    var content: MenuBarContent
    var icon: String
    var accounts: [WaybarAccount]
    var hasAccounts: Bool
    var failed: Bool
    var stale: Bool
    var percentage: Int
    var severity: WaybarSeverity
}

enum WaybarOutput {
    /// nf-md-speedometer. Waybar's usual Nerd Font has it; `--icon` replaces it.
    static let defaultIcon = "\u{F04C5}"
    static let barWidth = 8

    static func usedPercent(used: Double, limit: Double) -> Int {
        guard limit > 0, used.isFinite, limit.isFinite else { return 0 }
        return Int((min(max(used / limit, 0), 1) * 100).rounded())
    }

    static func strip(_ content: MenuBarContent, icon: String) -> String {
        guard !content.isEmpty else { return icon }
        let parts = content.items.map { item in
            let name = item.provider.badge.map { "\(item.provider.displayName) \($0)" } ?? item.provider.displayName
            return "\(name) \(item.shortLabel) \(item.value)"
        }
        var text = parts.joined(separator: " · ")
        if content.overflow > 0 { text += " +\(content.overflow)" }
        return text
    }

    static func bar(_ fraction: Double, width: Int = barWidth) -> String {
        let clamped = min(max(fraction, 0), 1)
        let filled = min(max(Int((clamped * Double(width)).rounded()), 0), width)
        return String(repeating: "█", count: filled) + String(repeating: "░", count: width - filled)
    }

    static func escape(_ text: String) -> String {
        text.replacingOccurrences(of: "&", with: "&amp;")
            .replacingOccurrences(of: "<", with: "&lt;")
            .replacingOccurrences(of: ">", with: "&gt;")
    }

    static func classes(for model: WaybarViewModel) -> [String] {
        guard model.hasAccounts else { return ["empty"] }
        var names = [model.severity.rawValue]
        if model.failed { names.append("error") }
        if model.stale { names.append("stale") }
        return names
    }

    static func tooltip(_ accounts: [WaybarAccount]) -> String {
        accounts.map(accountMarkup).joined(separator: "\n\n")
    }

    static func json(_ model: WaybarViewModel) -> String {
        let names = classes(for: model)
        let payload = Payload(
            text: strip(model.content, icon: model.icon),
            tooltip: tooltip(model.accounts),
            classes: names,
            percentage: model.percentage,
            alt: names.joined(separator: " ")
        )
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.withoutEscapingSlashes]
        guard let data = try? encoder.encode(payload), let text = String(data: data, encoding: .utf8) else {
            return #"{"text":"?","tooltip":"","class":["error"],"percentage":0,"alt":"error"}"#
        }
        return text
    }

    private static func accountMarkup(_ account: WaybarAccount) -> String {
        var header = "<b>\(escape(account.title))</b>"
        if let plan = account.plan, !plan.isEmpty { header += " · \(escape(plan))" }
        var lines = [header]
        lines.append(contentsOf: account.metrics.map(metricMarkup))
        if let problem = account.problem, !problem.isEmpty { lines.append(escape(problem)) }
        return lines.joined(separator: "\n")
    }

    private static func metricMarkup(_ metric: WaybarMetric) -> String {
        var parts = [escape(metric.title)]
        if let fraction = metric.fraction { parts.append(bar(fraction)) }
        parts.append(escape(metric.value))
        if let reset = metric.reset, !reset.isEmpty { parts.append(escape(reset)) }
        return parts.joined(separator: "  ")
    }

    private struct Payload: Encodable {
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
}
