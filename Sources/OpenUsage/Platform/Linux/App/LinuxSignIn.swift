import Foundation
#if canImport(Glibc)
import Glibc
#elseif canImport(Musl)
import Musl
#endif

@MainActor
enum LinuxSignIn {
    static func add(kind: ProviderKind, organization: String?) async -> Int32 {
        let method: ProviderAuthMethod = organization == nil ? kind.authMethods[0] : .organizationToken
        let (stream, continuation) = AsyncStream.makeStream(of: String.self)
        let reader = PasteReader(continuation)
        let cancellation = LoginCancellation()
        let interrupt = LinuxSignals.install(SIGINT) {
            cancellation.cancel()
            TerminalEcho.shared.restore()
            LinuxText.error("Sign-in cancelled.")
            exit(130)
        }
        defer { interrupt.cancel() }

        if case .apiKey = method {
            promptIfTerminal("API key: ")
            reader.start(echoOff: true)
        } else if method == .organizationToken {
            promptIfTerminal("Organization token: ")
            reader.start(echoOff: true)
        }

        let login = ProviderLoginCatalog.login(for: kind, method: method, organization: organization ?? "")
        let task = Task { () throws -> (label: String, credential: AccountCredential) in
            try await login.login(
                prompt: { prompt in present(prompt, reader: reader) },
                pastedCode: stream
            )
        }
        cancellation.store(task)
        let code: Int32
        do {
            let result = try await task.value
            try save(kind: kind, organization: organization, result: result)
            code = 0
        } catch LoginError.cancelled {
            code = cancelled(method: method, kind: kind)
        } catch is CancellationError {
            LinuxText.error("Sign-in was cancelled.")
            code = 130
        } catch {
            LinuxText.error(error.localizedDescription)
            code = 1
        }
        await reader.stop()
        return code
    }

    nonisolated private static func promptIfTerminal(_ text: String) {
        guard isatty(STDIN_FILENO) != 0 else { return }
        LinuxText.prompt(text)
    }

    nonisolated private static func present(_ prompt: LoginPrompt, reader: PasteReader) {
        switch prompt {
        case .browser(let url, let acceptsPastedCode):
            LinuxText.line(url.absoluteString)
            LinuxBrowser.open(url)
            if acceptsPastedCode {
                LinuxText.note("Paste the authorization code and press Enter, or finish in the browser.")
                reader.start(echoOff: false)
            } else {
                LinuxText.note("Finish sign-in in the browser.")
                reader.finishWithoutValue()
            }
        case .browserToken(let url):
            LinuxText.line(url.absoluteString)
            LinuxBrowser.open(url)
            promptIfTerminal("Token: ")
            reader.start(echoOff: true)
        case .deviceCode(let userCode, let url):
            LinuxText.line("Code: \(userCode)")
            LinuxText.line(url.absoluteString)
            LinuxBrowser.open(url)
            LinuxText.note("Confirm the code in the browser.")
            reader.finishWithoutValue()
        }
    }

    private static func save(
        kind: ProviderKind, organization: String?, result: (label: String, credential: AccountCredential)
    ) throws {
        let store = AccountStore(defaults: FileBackedUserDefaults.openUsage())
        try store.checkForDuplicate(result.credential, kind: kind, organization: organization)
        let account = store.add(
            kind: kind, label: result.label, organization: organization, credential: result.credential
        )
        guard store.accounts.contains(where: { $0.id == account.id }) else {
            throw ProviderError.unavailable(store.lastError ?? "The account could not be saved.")
        }
        LinuxText.line("Added \(account.cardID) (\(account.kind.displayName), \(account.label)).")
        WaybarDaemon.notifyRunning()
    }

    private static func cancelled(method: ProviderAuthMethod, kind: ProviderKind) -> Int32 {
        if case .apiKey = method {
            LinuxText.error("No API key was entered.")
            return 1
        }
        if method == .organizationToken {
            LinuxText.error("No organization token was entered.")
            return 1
        }
        if kind == .devin {
            LinuxText.error("No token was entered.")
            return 1
        }
        LinuxText.error("Sign-in was cancelled.")
        return 130
    }
}

private final class PasteReader: @unchecked Sendable {
    private let continuation: AsyncStream<String>.Continuation
    private let mutex = NSLock()
    private var started = false
    private var stopped = false
    private var task: Task<Void, Never>?

    init(_ continuation: AsyncStream<String>.Continuation) {
        self.continuation = continuation
    }

    func start(echoOff: Bool) {
        mutex.lock()
        let already = started
        started = true
        mutex.unlock()
        guard !already else { return }
        let continuation = continuation
        let task = Task.detached {
            let hideEcho = echoOff && isatty(STDIN_FILENO) != 0
            if hideEcho { TerminalEcho.shared.suppress() }
            defer { if hideEcho { TerminalEcho.shared.restore() } }
            var showedNewline = false
            while !self.isStopped {
                guard let line = Self.readLine(stop: { self.isStopped }) else { break }
                // Echo is off, so the Return key never moved the cursor.
                if hideEcho, !showedNewline {
                    FileHandle.standardError.write(Data("\n".utf8))
                    showedNewline = true
                }
                let trimmed = line.trimmingCharacters(in: .whitespacesAndNewlines)
                if !trimmed.isEmpty { continuation.yield(trimmed) }
            }
            continuation.finish()
        }
        mutex.lock()
        self.task = task
        mutex.unlock()
    }

    func finishWithoutValue() {
        continuation.finish()
    }

    func stop() async {
        await markStopped()?.value
    }

    private func markStopped() -> Task<Void, Never>? {
        mutex.lock()
        stopped = true
        let current = task
        mutex.unlock()
        return current
    }

    private var isStopped: Bool {
        mutex.lock()
        defer { mutex.unlock() }
        return stopped
    }

    /// One line, or nil at EOF. Returns nil promptly once `stop` is set, so a finished login does not
    /// leave the process blocked on the terminal.
    private static func readLine(stop: () -> Bool) -> String? {
        var buffer: [UInt8] = []
        var byte: UInt8 = 0
        while !stop() {
            var descriptor = pollfd()
            descriptor.fd = STDIN_FILENO
            descriptor.events = Int16(POLLIN)
            let ready = poll(&descriptor, 1, 200)
            if ready == 0 { continue }
            if ready < 0 {
                if errno == EINTR { continue }
                return nil
            }
            let count = Glibc.read(STDIN_FILENO, &byte, 1)
            if count == 0 { return buffer.isEmpty ? nil : String(bytes: buffer, encoding: .utf8) }
            if count < 0 {
                if errno == EINTR { continue }
                return nil
            }
            if byte == 10 { return String(bytes: buffer, encoding: .utf8) ?? "" }
            if byte != 13 { buffer.append(byte) }
        }
        return nil
    }
}

private final class LoginCancellation: @unchecked Sendable {
    private let mutex = NSLock()
    private var task: Task<(label: String, credential: AccountCredential), Error>?

    func store(_ task: Task<(label: String, credential: AccountCredential), Error>) {
        mutex.lock()
        self.task = task
        mutex.unlock()
    }

    func cancel() {
        mutex.lock()
        let task = task
        mutex.unlock()
        task?.cancel()
    }
}
