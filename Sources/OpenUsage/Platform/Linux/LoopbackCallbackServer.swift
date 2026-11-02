import Foundation
#if canImport(Glibc)
import Glibc
#elseif canImport(Musl)
import Musl
#endif

// The Linux counterpart of `Services/LoopbackCallbackServer.swift` (which uses Network.framework): the
// same OAuth redirect listener on 127.0.0.1, on a plain socket served from its own thread.
final class LoopbackCallbackServer: @unchecked Sendable {
    private let socket: Int32
    private let state: String
    private let path: String
    private let lock = NSLock()
    private var result: Result<String, Error>?
    private var continuation: CheckedContinuation<String, Error>?
    private var closed = false
    private(set) var redirectURI = ""

    private init(socket: Int32, state: String, path: String) {
        self.socket = socket
        self.state = state
        self.path = path
    }

    static func start(state: String, port: UInt16? = nil, path: String = "/callback") async throws -> LoopbackCallbackServer {
        let fd = Glibc.socket(AF_INET, Int32(SOCK_STREAM.rawValue), 0)
        guard fd >= 0 else { throw LoginError.listenerFailed(String(cString: strerror(errno))) }
        var reuse: Int32 = 1
        setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &reuse, socklen_t(MemoryLayout<Int32>.size))

        var address = sockaddr_in()
        address.sin_family = sa_family_t(AF_INET)
        address.sin_port = (port ?? 0).bigEndian
        address.sin_addr.s_addr = UInt32(0x7F00_0001).bigEndian
        let bound = withUnsafePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { bind(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size)) }
        }
        guard bound == 0, listen(fd, 8) == 0 else {
            let reason = String(cString: strerror(errno))
            close(fd)
            throw LoginError.listenerFailed(reason)
        }

        var assigned = sockaddr_in()
        var length = socklen_t(MemoryLayout<sockaddr_in>.size)
        let named = withUnsafeMutablePointer(to: &assigned) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { getsockname(fd, $0, &length) }
        }
        guard named == 0 else {
            close(fd)
            throw LoginError.listenerFailed("No local port was assigned.")
        }

        let server = LoopbackCallbackServer(socket: fd, state: state, path: path)
        server.redirectURI = "http://localhost:\(UInt16(bigEndian: assigned.sin_port))\(path)"
        let thread = Thread { [server] in server.acceptLoop() }
        thread.name = "com.robinebers.openusage.oauth-listener"
        thread.start()
        return server
    }

    func waitForCode() async throws -> String {
        try await withCheckedThrowingContinuation { continuation in
            lock.lock()
            if let result {
                lock.unlock()
                continuation.resume(with: result)
            } else {
                self.continuation = continuation
                lock.unlock()
            }
        }
    }

    func cancel() {
        finish(.failure(LoginError.cancelled))
        stopListening()
    }

    private func stopListening() {
        lock.lock()
        defer { lock.unlock() }
        guard !closed else { return }
        closed = true
        // Shutting the socket down wakes the blocked `accept`, which then ends the loop.
        shutdown(socket, Int32(SHUT_RDWR))
        close(socket)
    }

    private func acceptLoop() {
        while true {
            let connection = accept(socket, nil, nil)
            if connection < 0 {
                if errno == EINTR { continue }
                return
            }
            serve(connection)
            lock.lock()
            let done = result != nil
            lock.unlock()
            if done {
                stopListening()
                return
            }
        }
    }

    private static let headerLimit = 16_384
    private static let headerTerminator = Data("\r\n\r\n".utf8)

    /// The browser may deliver the request in pieces, so the header is parsed only once its terminator
    /// has arrived; a header cut off by the client or still unterminated at 16 KiB is refused.
    private func serve(_ connection: Int32) {
        defer { close(connection) }
        var timeout = timeval(tv_sec: 10, tv_usec: 0)
        setsockopt(connection, SOL_SOCKET, SO_RCVTIMEO, &timeout, socklen_t(MemoryLayout<timeval>.size))
        var buffered = Data()
        var chunk = [UInt8](repeating: 0, count: 4096)
        while buffered.count < Self.headerLimit {
            let count = recv(connection, &chunk, min(chunk.count, Self.headerLimit - buffered.count), 0)
            guard count > 0 else { break }
            buffered.append(contentsOf: chunk[..<count])
            if let end = buffered.range(of: Self.headerTerminator) {
                handle(String(decoding: buffered[..<end.lowerBound], as: UTF8.self), connection: connection)
                return
            }
        }
        reply(connection, status: 400, body: "Sign-in failed.", result: .failure(LoginError.invalidCallback))
    }

    private func handle(_ request: String, connection: Int32) {
        guard let line = request.components(separatedBy: "\r\n").first,
              let target = line.split(separator: " ").dropFirst().first,
              line.hasPrefix("GET ") else {
            reply(connection, status: 405, body: "Only GET is supported.")
            return
        }
        guard let url = URL(string: "http://localhost\(target)"), url.path == path else {
            reply(connection, status: 404, body: "Sign-in callback not found.")
            return
        }
        let query = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems ?? []
        let value = { (name: String) in query.first { $0.name == name }?.value }
        guard value("state") == state else {
            reply(connection, status: 400, body: "Sign-in state did not match.")
            return
        }
        if let error = value("error") {
            reply(connection, status: 400, body: "Sign-in failed.", result: .failure(LoginError.authorizationFailed(error)))
            return
        }
        guard let code = value("code"), !code.isEmpty else {
            reply(connection, status: 400, body: "Authorization code was missing.", result: .failure(LoginError.invalidCallback))
            return
        }
        reply(connection, status: 200, body: "Sign-in complete. You may close this page.", result: .success(code))
    }

    private func reply(_ connection: Int32, status: Int, body: String, result: Result<String, Error>? = nil) {
        if let result { finish(result) }
        let bodyData = Data("<!doctype html><title>OpenUsage</title><p>\(body)</p>".utf8)
        let response = Data(
            "HTTP/1.1 \(status) \(status == 200 ? "OK" : "Error")\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: \(bodyData.count)\r\nConnection: close\r\n\r\n".utf8
        ) + bodyData
        response.withUnsafeBytes { bytes in
            var sent = 0
            while sent < bytes.count {
                let count = send(connection, bytes.baseAddress! + sent, bytes.count - sent, Int32(MSG_NOSIGNAL))
                guard count > 0 else { return }
                sent += count
            }
        }
    }

    private func finish(_ result: Result<String, Error>) {
        lock.lock()
        guard self.result == nil else { lock.unlock(); return }
        self.result = result
        let pending = continuation
        continuation = nil
        lock.unlock()
        pending?.resume(with: result)
    }
}
