import CryptoKit
import XCTest
@testable import OpenUsage

final class SHA256ShimTests: XCTestCase {
    func testFIPS1804Vectors() {
        XCTAssertEqual(hex(SHA256.hash(data: Data())), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
        XCTAssertEqual(hex(SHA256.hash(data: Data("abc".utf8))), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        let fourHundredFortyEightBits = Data("abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq".utf8)
        XCTAssertEqual(fourHundredFortyEightBits.count, 56)
        XCTAssertEqual(
            hex(SHA256.hash(data: fourHundredFortyEightBits)),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        )
        let millionAs = Data(repeating: UInt8(ascii: "a"), count: 1_000_000)
        XCTAssertEqual(
            hex(SHA256.hash(data: millionAs)),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        )
    }

    func testPKCEAppendixBChallenge() {
        XCTAssertEqual(
            PKCE.challenge(for: "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        )
    }

    private func hex(_ digest: SHA256Digest) -> String {
        digest.map { String(format: "%02x", $0) }.joined()
    }
}
