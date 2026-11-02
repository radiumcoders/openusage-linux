import Foundation
#if canImport(Glibc)
import Glibc
#elseif canImport(Musl)
import Musl
#endif

struct WaybarLockError: LocalizedError {
    var message: String
    var errorDescription: String? { message }
}

final class WaybarPIDLock: @unchecked Sendable {
    private let fd: Int32
    private let url: URL

    static func acquire() throws -> WaybarPIDLock {
        let url = OpenUsagePaths.waybarPID
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        let fd = open(url.path, O_RDWR | O_CREAT, mode_t(0o644))
        guard fd >= 0 else {
            throw WaybarLockError(message: "Could not create the Waybar pid file: \(String(cString: strerror(errno)))")
        }
        if flock(fd, LOCK_EX | LOCK_NB) != 0 {
            let blocked = errno == EWOULDBLOCK || errno == EAGAIN
            let existing = try? String(contentsOf: url, encoding: .utf8)
            let pid = existing.flatMap { Int32($0.trimmingCharacters(in: .whitespacesAndNewlines)) }
            close(fd)
            if blocked {
                throw WaybarLockError(
                    message: pid.map { "Waybar module is already running (pid \($0))." } ?? "Waybar module is already running."
                )
            }
            throw WaybarLockError(message: "Could not lock the Waybar pid file: \(String(cString: strerror(errno)))")
        }
        let text = "\(getpid())\n"
        _ = ftruncate(fd, 0)
        _ = lseek(fd, 0, SEEK_SET)
        _ = text.withCString { write(fd, $0, strlen($0)) }
        return WaybarPIDLock(fd: fd, url: url)
    }

    func release() {
        unlink(url.path)
        flock(fd, LOCK_UN)
        close(fd)
    }

    private init(fd: Int32, url: URL) {
        self.fd = fd
        self.url = url
    }
}

enum WaybarDaemon {
    /// Tells a running daemon to reload its accounts (SIGUSR2).
    static func notifyRunning() {
        _ = signalRunning(SIGUSR2)
    }

    /// Sends `signal` to the daemon named in the pid file, if it is running. A pid file with nobody
    /// holding its lock is stale.
    static func signalRunning(_ signal: Int32) -> Bool {
        let url = OpenUsagePaths.waybarPID
        let fd = open(url.path, O_RDWR)
        guard fd >= 0 else { return false }
        defer { close(fd) }
        if flock(fd, LOCK_EX | LOCK_NB) == 0 {
            flock(fd, LOCK_UN)
            return false
        }
        guard let text = try? String(contentsOf: url, encoding: .utf8),
              let pid = Int32(text.trimmingCharacters(in: .whitespacesAndNewlines)), pid > 0 else { return false }
        return kill(pid, signal) == 0
    }

    @MainActor
    static func run(once: Bool, icon: String) async -> Int32 {
        let session = LinuxSession()
        if once {
            emit(session.waybarLine(icon: icon))
            return 0
        }
        let lock: WaybarPIDLock
        do { lock = try WaybarPIDLock.acquire() } catch {
            LinuxText.error(error.localizedDescription)
            return 1
        }
        defer { lock.release() }

        let control = DaemonControl()
        // SIGUSR1 is the user's refresh (a click on the module), which skips the cache like the Mac
        // app's Command-R. SIGUSR2 comes from `add`, `remove`, and `rename`: reload the accounts and
        // fetch only what isn't fresh, so a new account is read without re-reading the others.
        let refreshSource = LinuxSignals.install(SIGUSR1) { @Sendable in control.requestReload(force: true) }
        let reloadSource = LinuxSignals.install(SIGUSR2) { @Sendable in control.requestReload(force: false) }
        let interrupt = LinuxSignals.install(SIGINT) { @Sendable in control.requestStop() }
        let terminate = LinuxSignals.install(SIGTERM) { @Sendable in control.requestStop() }
        defer {
            refreshSource.cancel()
            reloadSource.cancel()
            interrupt.cancel()
            terminate.cancel()
        }

        let quotas = QuotaNotificationEvaluator()
        // Waybar restarts this on every config reload, so launch refreshes like the Mac app does: an
        // account read within the last interval keeps its cached reading.
        await session.hubs.syncDue()
        await session.refresh(force: false)
        await session.evaluateQuotas(quotas)
        emit(session.waybarLine(icon: icon))

        let interval = RefreshSetting.interval
        var nextRefresh = ContinuousClock.now.advanced(by: .seconds(interval))
        var hourlyAt = ContinuousClock.now.advanced(by: .seconds(interval / 2))
        var hourlyDone = false

        while !control.isStopped {
            if let force = control.takeReload() ?? (session.accountsChangedOnDisk() ? false : nil) {
                session.reloadFromDisk()
                await session.refresh(force: force)
                await session.evaluateQuotas(quotas)
                emit(session.waybarLine(icon: icon))
                let started = ContinuousClock.now
                nextRefresh = started.advanced(by: .seconds(interval))
                hourlyAt = started.advanced(by: .seconds(interval / 2))
                hourlyDone = false
                continue
            }
            let now = ContinuousClock.now
            if now >= nextRefresh {
                await session.hubs.syncDue()
                if session.accountsChangedOnDisk() { continue }
                await session.refresh(force: false)
                await session.evaluateQuotas(quotas)
                emit(session.waybarLine(icon: icon))
                let started = ContinuousClock.now
                nextRefresh = started.advanced(by: .seconds(interval))
                hourlyAt = started.advanced(by: .seconds(interval / 2))
                hourlyDone = false
                continue
            }
            if !hourlyDone, now >= hourlyAt {
                await session.data.refreshHourlyReads()
                emit(session.waybarLine(icon: icon))
                hourlyDone = true
                continue
            }
            let tick = now.advanced(by: .seconds(30))
            let wake = [tick, nextRefresh, hourlyDone ? nextRefresh : hourlyAt].min() ?? tick
            await control.sleep(until: wake)
            if control.isStopped { break }
            let woke = ContinuousClock.now
            if !control.reloadPending, !session.accountsChangedOnDisk(), woke < nextRefresh, hourlyDone || woke < hourlyAt {
                emit(session.waybarLine(icon: icon))
            }
        }
        return 0
    }

    private static func emit(_ json: String) {
        FileHandle.standardOutput.write(Data(json.utf8))
        FileHandle.standardOutput.write(Data("\n".utf8))
    }
}

private final class DaemonControl: @unchecked Sendable {
    private let mutex = NSLock()
    /// A pending reload, and whether it skips the cache.
    private var reload: Bool?
    private var stop = false
    private var sleeper: Task<Void, Never>?

    var isStopped: Bool {
        mutex.lock()
        defer { mutex.unlock() }
        return stop
    }

    var reloadPending: Bool {
        mutex.lock()
        defer { mutex.unlock() }
        return reload != nil
    }

    func requestReload(force: Bool) {
        mutex.lock()
        reload = (reload ?? false) || force
        let pending = sleeper
        mutex.unlock()
        pending?.cancel()
    }

    func requestStop() {
        mutex.lock()
        stop = true
        let pending = sleeper
        mutex.unlock()
        pending?.cancel()
    }

    func takeReload() -> Bool? {
        mutex.lock()
        defer { mutex.unlock() }
        let value = reload
        reload = nil
        return value
    }

    func sleep(until deadline: ContinuousClock.Instant) async {
        let task = Task<Void, Never> {
            do { try await Task.sleep(until: deadline, clock: .continuous) } catch {}
        }
        if beginSleep(task) { task.cancel() }
        await task.value
        endSleep(task)
    }

    private func beginSleep(_ task: Task<Void, Never>) -> Bool {
        mutex.lock()
        sleeper = task
        let already = reload != nil || stop
        mutex.unlock()
        return already
    }

    private func endSleep(_ task: Task<Void, Never>) {
        mutex.lock()
        if sleeper == task { sleeper = nil }
        mutex.unlock()
    }
}
