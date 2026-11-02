import Foundation
#if canImport(Glibc)
import Glibc
#elseif canImport(Musl)
import Musl
#endif

// Two long-lived processes (the Waybar daemon and a one-shot `openusage` command) share account
// settings and the usage cache. Linux UserDefaults keeps a per-process cache and rewrites a whole
// file, so one command can wipe the other's keys. These files are the store instead: every read
// reloads under flock, and every write reloads, edits, and renames a temp file into place.
//
// Swift 6.2's UserDefaults implements `data` / `string` / `bool` / `stringArray` by calling
// `object(forKey:)`, and the typed setters call `set(_:forKey:)`. Overriding those three methods
// is enough for AccountStore, LayoutStore, WidgetDataStore, and ProviderSnapshotCache.

enum OpenUsageStorageKey {
    /// Matches `ProviderSnapshotCache`'s storage key. Anything else lives in the settings file.
    static let snapshots = "openusage.providerSnapshots.v11"
}

enum OpenUsagePaths {
    static var settingsFile: URL { configDirectory().appendingPathComponent("settings.plist") }
    static var snapshotsFile: URL { stateDirectory().appendingPathComponent("snapshots.plist") }
    static var waybarPID: URL { runtimeDirectory().appendingPathComponent("waybar.pid") }

    static func configDirectory(
        environment: [String: String] = ProcessInfo.processInfo.environment, home: String = NSHomeDirectory()
    ) -> URL {
        root(environment: environment, key: "XDG_CONFIG_HOME", home: home, fallback: ".config")
            .appendingPathComponent("openusage", isDirectory: true)
    }

    static func stateDirectory(
        environment: [String: String] = ProcessInfo.processInfo.environment, home: String = NSHomeDirectory()
    ) -> URL {
        root(environment: environment, key: "XDG_STATE_HOME", home: home, fallback: ".local/state")
            .appendingPathComponent("openusage", isDirectory: true)
    }

    static func runtimeDirectory(
        environment: [String: String] = ProcessInfo.processInfo.environment, home: String = NSHomeDirectory()
    ) -> URL {
        let base = root(
            environment: environment, key: "XDG_RUNTIME_DIR", home: home,
            fallback: nil, emptyFallback: "/tmp/openusage-\(getuid())"
        )
        return base.appendingPathComponent("openusage", isDirectory: true)
    }

    static func root(
        environment: [String: String], key: String, home: String, fallback: String?, emptyFallback: String? = nil
    ) -> URL {
        if let value = environment[key], !value.isEmpty { return URL(fileURLWithPath: value, isDirectory: true) }
        if let emptyFallback { return URL(fileURLWithPath: emptyFallback, isDirectory: true) }
        return URL(fileURLWithPath: home, isDirectory: true).appendingPathComponent(fallback ?? "", isDirectory: true)
    }
}

final class FileBackedUserDefaults: UserDefaults {
    private let settings: PlistFile
    private let snapshots: PlistFile

    init(settingsFile: URL, snapshotsFile: URL) {
        settings = PlistFile(url: settingsFile)
        snapshots = PlistFile(url: snapshotsFile)
        // The suite is never read or written; the overrides below own every key.
        super.init(suiteName: "openusage.file-backed")!
    }

    static func openUsage() -> FileBackedUserDefaults {
        FileBackedUserDefaults(settingsFile: OpenUsagePaths.settingsFile, snapshotsFile: OpenUsagePaths.snapshotsFile)
    }

    override func object(forKey defaultName: String) -> Any? {
        file(for: defaultName).object(forKey: defaultName).map(Self.coerce)
    }

    override func set(_ value: Any?, forKey defaultName: String) {
        file(for: defaultName).set(value, forKey: defaultName)
    }

    override func removeObject(forKey defaultName: String) {
        file(for: defaultName).removeObject(forKey: defaultName)
    }

    override func synchronize() -> Bool { true }

    private func file(for key: String) -> PlistFile {
        key == OpenUsageStorageKey.snapshots ? snapshots : settings
    }

    /// Property-list bools can come back as `__NSCFBoolean` (`objCType` "c"). Callers such as
    /// `bool(forKey:default:)` only accept a real `Bool`.
    static func coerce(_ value: Any) -> Any {
        if let number = value as? NSNumber {
            let type = String(cString: number.objCType)
            if type == "c" || type == "B" { return number.boolValue }
        }
        if let data = value as? Data { return Data(data) }
        if let text = value as? String { return text }
        if let items = value as? [Any] { return items.map(coerce) }
        if let entries = value as? [String: Any] { return entries.mapValues(coerce) }
        return value
    }
}

private struct PlistFile: Sendable {
    let url: URL

    func object(forKey key: String) -> Any? {
        withLock(exclusive: false) { load().values[key] } ?? nil
    }

    func set(_ value: Any?, forKey key: String) {
        mutate { values in
            if let value, !(value is NSNull) { values[key] = value } else { values.removeValue(forKey: key) }
        }
    }

    func removeObject(forKey key: String) {
        mutate { $0.removeValue(forKey: key) }
    }

    private func mutate(_ body: (inout [String: Any]) -> Void) {
        _ = withLock(exclusive: true) { () -> Void in
            var loaded = load()
            guard !loaded.unreadable else { return }
            body(&loaded.values)
            store(loaded.values)
        }
    }

    private func load() -> (values: [String: Any], unreadable: Bool) {
        guard FileManager.default.fileExists(atPath: url.path) else { return ([:], false) }
        guard let data = try? Data(contentsOf: url), !data.isEmpty else { return ([:], false) }
        do {
            var format = PropertyListSerialization.PropertyListFormat.xml
            let object = try PropertyListSerialization.propertyList(from: data, options: [], format: &format)
            guard let values = object as? [String: Any] else {
                AppLog.error(.config, "\(url.lastPathComponent) is not a property list dictionary")
                return ([:], true)
            }
            return (values, false)
        } catch {
            AppLog.error(.config, "could not read \(url.lastPathComponent): \(error.localizedDescription)")
            return ([:], true)
        }
    }

    private func store(_ values: [String: Any]) {
        let data: Data
        do {
            data = try PropertyListSerialization.data(fromPropertyList: values, format: .xml, options: 0)
        } catch {
            AppLog.error(.config, "could not encode \(url.lastPathComponent): \(error.localizedDescription)")
            return
        }
        do {
            try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        } catch {
            AppLog.error(.config, "could not create \(url.deletingLastPathComponent().path): \(error.localizedDescription)")
            return
        }
        let tmp = url.deletingLastPathComponent()
            .appendingPathComponent(".\(url.lastPathComponent).\(getpid()).\(UUID().uuidString).tmp")
        let fd = open(tmp.path, O_WRONLY | O_CREAT | O_TRUNC, mode_t(0o644))
        guard fd >= 0 else {
            AppLog.error(.config, "could not write \(url.lastPathComponent): \(String(cString: strerror(errno)))")
            return
        }
        var pending = data
        let wrote = pending.withUnsafeMutableBytes { raw -> Bool in
            guard let base = raw.baseAddress else { return raw.count == 0 }
            var offset = 0
            while offset < raw.count {
                let count = write(fd, base.advanced(by: offset), raw.count - offset)
                if count < 0 {
                    if errno == EINTR { continue }
                    return false
                }
                offset += count
            }
            return fsync(fd) == 0
        }
        close(fd)
        guard wrote, rename(tmp.path, url.path) == 0 else {
            AppLog.error(.config, "could not replace \(url.lastPathComponent): \(String(cString: strerror(errno)))")
            unlink(tmp.path)
            return
        }
    }

    private func withLock<T>(exclusive: Bool, _ body: () -> T) -> T? {
        let directory = url.deletingLastPathComponent()
        try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let lockURL = directory.appendingPathComponent(".\(url.lastPathComponent).lock")
        let fd = open(lockURL.path, O_RDWR | O_CREAT, mode_t(0o644))
        guard fd >= 0 else {
            AppLog.error(.config, "could not lock \(url.lastPathComponent): \(String(cString: strerror(errno)))")
            return nil
        }
        let operation = exclusive ? LOCK_EX : LOCK_SH
        while flock(fd, operation) != 0 {
            if errno == EINTR { continue }
            AppLog.error(.config, "could not lock \(url.lastPathComponent): \(String(cString: strerror(errno)))")
            close(fd)
            return nil
        }
        let value = body()
        flock(fd, LOCK_UN)
        close(fd)
        return value
    }
}
