import XCTest
@testable import OpenUsage

/// The Mac messages point at the account's page; Linux has no window, so they name the commands instead.
final class LinuxErrorWordingTests: XCTestCase {
    func testExpiredSignInNamesTheAddCommand() {
        XCTAssertEqual(
            ProviderError.signInExpired(.claude).errorDescription,
            "Claude sign-in expired. Sign in again with `openusage add claude` to keep reading usage."
        )
    }

    func testRejectedKeyNamesRemoveAndAdd() {
        XCTAssertEqual(
            ProviderError.keyRejected("OpenRouter rejected the API key.").errorDescription,
            "OpenRouter rejected the API key. Remove the account and add it again with a new one (`openusage remove`, `openusage add`)."
        )
    }
}
