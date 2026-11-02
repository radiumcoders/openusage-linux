import Foundation

// The Linux counterpart of `Support/AppNotifications.swift`: quota alerts go to the desktop's
// notification daemon (mako, dunst, swaync, …) through `notify-send`.
@MainActor
final class AppNotifications {
    static let shared = AppNotifications()

    func post(idPrefix: String, title: String, subtitle: String, body: String) async -> Bool {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/env")
        process.arguments = ["notify-send", "--app-name=OpenUsage", "--icon=dialog-information", title, "\(subtitle)\n\(body)"]
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        let status: Int32? = await withCheckedContinuation { continuation in
            process.terminationHandler = { continuation.resume(returning: $0.terminationStatus) }
            do {
                try process.run()
            } catch {
                process.terminationHandler = nil
                AppLog.warn(.notifications, "skip \(idPrefix): notify-send could not run: \(error.localizedDescription)")
                continuation.resume(returning: nil)
            }
        }
        guard let status else { return false }
        guard status == 0 else {
            AppLog.warn(.notifications, "skip \(idPrefix): notify-send exited \(status)")
            return false
        }
        AppLog.debug(.notifications, "posted \(idPrefix)")
        return true
    }
}
