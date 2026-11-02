// Linux stand-in for the parts of Apple's `os` module the shared OpenUsage code uses: the unfair lock
// and unified logging. Log lines go to stderr, which Waybar and systemd hand to the journal.
import Foundation
import Synchronization

public final class OSAllocatedUnfairLock<State: ~Copyable>: @unchecked Sendable {
    private let mutex: Mutex<State>

    public init(initialState: consuming sending State) {
        mutex = Mutex(initialState)
    }

    public func withLock<R: ~Copyable, E: Error>(_ body: (inout sending State) throws(E) -> sending R) throws(E) -> sending R {
        try mutex.withLock(body)
    }
}

public enum OSLogType: Sendable {
    case `default`, info, debug, error, fault

    fileprivate var label: String {
        switch self {
        case .default: "notice"
        case .info: "info"
        case .debug: "debug"
        case .error: "error"
        case .fault: "fault"
        }
    }
}

public struct OSLogPrivacy: Sendable {
    public static let `public` = OSLogPrivacy()
    public static let `private` = OSLogPrivacy()
}

public struct OSLogMessage: ExpressibleByStringInterpolation, Sendable {
    public struct StringInterpolation: StringInterpolationProtocol, Sendable {
        fileprivate var text = ""

        public init(literalCapacity: Int, interpolationCount: Int) {
            text.reserveCapacity(literalCapacity)
        }

        public mutating func appendLiteral(_ literal: String) {
            text += literal
        }

        public mutating func appendInterpolation(_ value: @autoclosure () -> String, privacy: OSLogPrivacy = .private) {
            text += value()
        }

        public mutating func appendInterpolation<T: CustomStringConvertible>(_ value: @autoclosure () -> T, privacy: OSLogPrivacy = .private) {
            text += value().description
        }
    }

    fileprivate let text: String

    public init(stringLiteral value: String) {
        text = value
    }

    public init(stringInterpolation: StringInterpolation) {
        text = stringInterpolation.text
    }
}

public struct Logger: Sendable {
    private let subsystem: String
    private let category: String
    private static let output = Mutex(())

    public init(subsystem: String, category: String) {
        self.subsystem = subsystem
        self.category = category
    }

    public func log(level: OSLogType, _ message: OSLogMessage) {
        let line = "\(subsystem) \(level.label): \(message.text)\n"
        Self.output.withLock { _ in
            FileHandle.standardError.write(Data(line.utf8))
        }
    }

    public func error(_ message: OSLogMessage) { log(level: .error, message) }
    public func warning(_ message: OSLogMessage) { log(level: .default, message) }
    public func notice(_ message: OSLogMessage) { log(level: .default, message) }
    public func info(_ message: OSLogMessage) { log(level: .info, message) }
    public func debug(_ message: OSLogMessage) { log(level: .debug, message) }
}
