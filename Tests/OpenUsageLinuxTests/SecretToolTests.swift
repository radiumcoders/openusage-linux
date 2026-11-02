import Foundation
import XCTest
@testable import OpenUsage

/// `secret-tool` stand-in. `store` writes stdin to a file, `lookup` prints it or exits 1 with no
/// output, `clear` removes it or exits 1 with no output. A `fail` file beside the script makes every
/// command write to stderr and exit 1.
final class SecretToolTests: XCTestCase {
    private var root: URL?

    override func setUpWithError() throws {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("openusage-secret-tool-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        let script = root.appendingPathComponent("secret-tool")
        try Self.scriptSource.write(to: script, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: script.path)
        self.root = root
    }

    override func tearDownWithError() throws {
        if let root {
            try? FileManager.default.removeItem(at: root)
        }
        root = nil
    }

    func testLoadSaveUpdateAndDeleteRoundTripThroughTheFakeKeyring() throws {
        let vault = try makeVault()
        let id = UUID()
        XCTAssertNil(try vault.load(id))

        let apiKey = AccountCredential.apiKey("sk-test")
        try vault.save(apiKey, for: id)
        XCTAssertEqual(try vault.load(id), apiKey)

        let missing = UUID()
        XCTAssertThrowsError(try vault.update(apiKey, for: missing)) { error in
            guard case CredentialVaultError.missingCredential = error else {
                return XCTFail("expected a missing credential, got \(error)")
            }
        }

        let oauth = AccountCredential.oauth(OAuthTokens(
            accessToken: "access",
            refreshToken: "refresh",
            expiresAt: Date(timeIntervalSince1970: 1_700_000_000),
            extras: [OAuthTokens.identityKey: "user@example.com"]
        ))
        try vault.update(oauth, for: id)
        XCTAssertEqual(try vault.load(id), oauth)

        try vault.delete(id)
        XCTAssertNil(try vault.load(id))
        try vault.delete(id)
    }

    func testAToolErrorSurfacesAsKeychainVaultError() throws {
        let vault = try makeVault()
        try Data().write(to: try rootURL().appendingPathComponent("fail"))
        XCTAssertThrowsError(try vault.save(.apiKey("sk-test"), for: UUID())) { error in
            guard case KeychainVaultError.failed(let reason) = error else {
                return XCTFail("expected a keyring failure, got \(error)")
            }
            XCTAssertTrue(reason.contains("keyring unavailable"), reason)
        }
        XCTAssertThrowsError(try vault.load(UUID())) { error in
            guard case KeychainVaultError.failed = error else {
                return XCTFail("expected a keyring failure, got \(error)")
            }
        }
    }

    private func makeVault() throws -> KeychainCredentialVault {
        var vault = KeychainCredentialVault()
        vault.tool.executable = try rootURL().appendingPathComponent("secret-tool").path
        return vault
    }

    private func rootURL() throws -> URL {
        try XCTUnwrap(root)
    }

    private static let scriptSource = #"""
    #!/bin/sh
    here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd) || exit 1
    if [ -f "$here/fail" ]; then
      printf '%s\n' 'keyring unavailable' >&2
      exit 1
    fi
    mkdir -p "$here/secrets"
    cmd=${1:-}
    if [ -z "$cmd" ]; then
      printf '%s\n' 'missing command' >&2
      exit 1
    fi
    shift
    service=
    account=
    while [ $# -gt 0 ]; do
      case $1 in
        --label=*) ;;
        service)
          service=${2:-}
          shift
          ;;
        account)
          account=${2:-}
          shift
          ;;
        *)
          printf '%s\n' "unexpected argument" >&2
          exit 1
          ;;
      esac
      shift
    done
    file=$here/secrets/${service}__${account}
    case $cmd in
      store)
        cat > "$file"
        ;;
      lookup)
        if [ -f "$file" ]; then
          cat "$file"
          exit 0
        fi
        exit 1
        ;;
      clear)
        if [ -f "$file" ]; then
          rm -f "$file"
          exit 0
        fi
        exit 1
        ;;
      *)
        printf '%s\n' 'unknown command' >&2
        exit 1
        ;;
    esac
    """#
}
