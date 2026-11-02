import Foundation
#if canImport(Glibc)
import Glibc
#elseif canImport(Musl)
import Musl
#endif

enum LinuxText {
    static func error(_ message: String) {
        write(FileHandle.standardError, "openusage: \(message)\n")
    }

    static func note(_ message: String) {
        write(FileHandle.standardError, message.hasSuffix("\n") ? message : message + "\n")
    }

    static func prompt(_ message: String) {
        write(FileHandle.standardError, message)
    }

    static func line(_ message: String) {
        write(FileHandle.standardOutput, message.hasSuffix("\n") ? message : message + "\n")
    }

    private static func write(_ handle: FileHandle, _ text: String) {
        // FileHandle writes the descriptor directly, so a pipe sees each line without an extra flush.
        handle.write(Data(text.utf8))
    }
}

enum LinuxSignals {
    @discardableResult
    static func install(_ signal: Int32, _ handler: @escaping @Sendable () -> Void) -> DispatchSourceSignal {
        _ = Glibc.signal(signal, SIG_IGN)
        let source = DispatchSource.makeSignalSource(signal: signal, queue: .global())
        source.setEventHandler(handler: handler)
        source.resume()
        return source
    }
}

final class TerminalEcho: @unchecked Sendable {
    static let shared = TerminalEcho()
    private let mutex = NSLock()
    private var saved: termios?

    func suppress() {
        guard isatty(STDIN_FILENO) != 0 else { return }
        var original = termios()
        guard tcgetattr(STDIN_FILENO, &original) == 0 else { return }
        mutex.lock()
        if saved == nil { saved = original }
        mutex.unlock()
        var hidden = original
        hidden.c_lflag &= ~tcflag_t(ECHO)
        tcsetattr(STDIN_FILENO, TCSANOW, &hidden)
    }

    func restore() {
        mutex.lock()
        let original = saved
        saved = nil
        mutex.unlock()
        guard var original else { return }
        tcsetattr(STDIN_FILENO, TCSANOW, &original)
    }
}

enum LinuxBrowser {
    static func open(_ url: URL) {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/env")
        process.arguments = ["xdg-open", url.absoluteString]
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        do {
            try process.run()
        } catch {
            AppLog.warn(.auth, "could not open a browser: \(error.localizedDescription)")
            LinuxText.note("Could not open a browser. Use the URL above.")
            return
        }
        Task.detached { process.waitUntilExit() }
    }
}
