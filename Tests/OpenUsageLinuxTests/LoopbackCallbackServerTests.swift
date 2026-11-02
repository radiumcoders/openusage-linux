import Foundation
import XCTest
#if canImport(Glibc)
import Glibc
#elseif canImport(Musl)
import Musl
#endif
@testable import OpenUsage

final class LoopbackCallbackServerTests: XCTestCase {
    func testWaitForCodeReturnsTheCodeForTheMatchingState() async throws {
        let server = try await LoopbackCallbackServer.start(state: "expected", port: nil)
        defer { server.cancel() }
        let pending = Task { try await server.waitForCode() }
        let response = try CallbackHTTP.get(redirectURI: server.redirectURI, query: "code=accepted&state=expected")
        XCTAssertEqual(response.status, 200, response.raw)
        let code = try await pending.value
        XCTAssertEqual(code, "accepted")
    }

    func testWrongStateIs400AndLeavesTheSignInWaiting() async throws {
        let server = try await LoopbackCallbackServer.start(state: "expected", port: nil)
        defer { server.cancel() }
        let pending = Task { try await server.waitForCode() }
        let rejected = try CallbackHTTP.get(redirectURI: server.redirectURI, query: "code=nope&state=wrong")
        XCTAssertEqual(rejected.status, 400, rejected.raw)
        let accepted = try CallbackHTTP.get(redirectURI: server.redirectURI, query: "code=later&state=expected")
        XCTAssertEqual(accepted.status, 200, accepted.raw)
        let code = try await pending.value
        XCTAssertEqual(code, "later")
    }

    func testAnErrorParameterFailsTheWait() async throws {
        let server = try await LoopbackCallbackServer.start(state: "expected", port: nil)
        defer { server.cancel() }
        let pending = Task { try await server.waitForCode() }
        let response = try CallbackHTTP.get(redirectURI: server.redirectURI, query: "error=access_denied&state=expected")
        XCTAssertEqual(response.status, 400, response.raw)
        do {
            _ = try await pending.value
            XCTFail("expected the error query to fail sign-in")
        } catch {
            XCTAssertEqual(error.localizedDescription, LoginError.authorizationFailed("access_denied").localizedDescription)
        }
    }

    func testCancelFailsTheWait() async throws {
        let server = try await LoopbackCallbackServer.start(state: "expected", port: nil)
        let pending = Task { try await server.waitForCode() }
        server.cancel()
        do {
            _ = try await pending.value
            XCTFail("expected cancel to fail sign-in")
        } catch {
            XCTAssertEqual(error.localizedDescription, LoginError.cancelled.localizedDescription)
        }
    }
}

private enum CallbackHTTP {
    struct Response {
        var status: Int
        var raw: String
    }

    static func get(redirectURI: String, query: String) throws -> Response {
        guard let url = URL(string: redirectURI), let port = url.port else {
            throw URLError(.badURL)
        }
        var last = URLError(.cannotConnectToHost)
        for _ in 0..<50 {
            do {
                return try exchange(port: UInt16(port), target: "\(url.path)?\(query)")
            } catch is ConnectionRefused {
                last = URLError(.cannotConnectToHost)
                Thread.sleep(forTimeInterval: 0.02)
            }
        }
        throw last
    }

    private struct ConnectionRefused: Error {}

    private static func exchange(port: UInt16, target: String) throws -> Response {
        let fd = Glibc.socket(AF_INET, Int32(SOCK_STREAM.rawValue), 0)
        guard fd >= 0 else { throw posixError() }
        defer { _ = Glibc.close(fd) }
        var timeout = timeval(tv_sec: 5, tv_usec: 0)
        setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, socklen_t(MemoryLayout<timeval>.size))
        setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &timeout, socklen_t(MemoryLayout<timeval>.size))

        var address = sockaddr_in()
        address.sin_family = sa_family_t(AF_INET)
        address.sin_port = port.bigEndian
        address.sin_addr.s_addr = UInt32(0x7F00_0001).bigEndian
        let connected = withUnsafePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                Glibc.connect(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
            }
        }
        guard connected == 0 else {
            if errno == ECONNREFUSED { throw ConnectionRefused() }
            throw posixError()
        }

        let request = Data("GET \(target) HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n".utf8)
        try request.withUnsafeBytes { buffer in
            var sent = 0
            while sent < buffer.count {
                let count = Glibc.send(fd, buffer.baseAddress! + sent, buffer.count - sent, 0)
                guard count > 0 else { throw posixError() }
                sent += count
            }
        }

        var raw = Data()
        var chunk = [UInt8](repeating: 0, count: 4096)
        while raw.count < 65_536 {
            let count = Glibc.recv(fd, &chunk, chunk.count, 0)
            if count == 0 { break }
            if count < 0 {
                if errno == EINTR { continue }
                throw posixError()
            }
            raw.append(contentsOf: chunk[..<count])
        }
        let text = String(decoding: raw, as: UTF8.self)
        guard let line = text.split(separator: "\r\n", maxSplits: 1).first else {
            throw URLError(.badServerResponse)
        }
        let parts = line.split(separator: " ")
        guard parts.count >= 2, let status = Int(parts[1]) else {
            throw URLError(.badServerResponse)
        }
        return Response(status: status, raw: text)
    }

    private static func posixError() -> NSError {
        NSError(
            domain: NSPOSIXErrorDomain,
            code: Int(errno),
            userInfo: [NSLocalizedDescriptionKey: String(cString: strerror(errno))]
        )
    }
}
