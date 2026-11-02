import XCTest
@testable import OpenUsage

final class ProxyConfigLinuxTests: XCTestCase {
    func testCurlProxyURLRewritesSOCKSAndKeepsHTTPSchemesWithCredentials() throws {
        let cases: [(url: String, expected: ProxyConfig, curl: String)] = [
            (
                "socks5://user:secret@127.0.0.1:1080",
                ProxyConfig(scheme: .socks5, host: "127.0.0.1", port: 1080, credential: .init(username: "user", password: "secret")),
                "socks5h://user:secret@127.0.0.1:1080"
            ),
            (
                "http://user:secret@proxy.example.com:8080",
                ProxyConfig(scheme: .http, host: "proxy.example.com", port: 8080, credential: .init(username: "user", password: "secret")),
                "http://user:secret@proxy.example.com:8080"
            ),
            (
                "https://user:secret@proxy.example.com:8443",
                ProxyConfig(scheme: .https, host: "proxy.example.com", port: 8443, credential: .init(username: "user", password: "secret")),
                "https://user:secret@proxy.example.com:8443"
            ),
            (
                "http://us%40er:p%40ss@proxy.example.com",
                ProxyConfig(scheme: .http, host: "proxy.example.com", port: 80, credential: .init(username: "us@er", password: "p@ss")),
                "http://us%40er:p%40ss@proxy.example.com:80"
            )
        ]
        for (url, expected, curl) in cases {
            let config = try XCTUnwrap(try ProxyConfig.parse(Self.file(url: url)), url)
            XCTAssertEqual(config, expected, url)
            XCTAssertEqual(config.curlProxyURL, curl, url)
        }
    }

    func testADisabledProxyIsNil() throws {
        let texts = [
            "{}",
            #"{"proxy":{"enabled":false}}"#,
            #"{"proxy":{"enabled":false,"url":"socks5://user:secret@127.0.0.1:1080"}}"#
        ]
        for text in texts {
            XCTAssertNil(try ProxyConfig.parse(Data(text.utf8)), text)
        }
    }

    private static func file(url: String) -> Data {
        let object: [String: Any] = ["proxy": ["enabled": true, "url": url]]
        return try! JSONSerialization.data(withJSONObject: object)
    }
}
