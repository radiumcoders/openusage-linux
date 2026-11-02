import Foundation
import os

// The Linux counterpart of `Services/AccountCredentialVault.swift`: the same vault contract, backed by
// the desktop keyring (the freedesktop Secret Service, e.g. GNOME Keyring or KWallet) through libsecret's
// `secret-tool`, under the same service name the Mac app gives its Keychain items.

protocol CredentialVault: Sendable {
    func load(_ id: UUID) throws -> AccountCredential?
    func save(_ credential: AccountCredential, for id: UUID) throws
    func update(_ credential: AccountCredential, for id: UUID) throws
    func delete(_ id: UUID) throws
}

/// Holds one sign-in in memory, so a new one proves itself without seeing the sign-in it would replace.
struct ProvisionalCredentialVault: CredentialVault {
    private let stored: OSAllocatedUnfairLock<AccountCredential?>

    init(_ credential: AccountCredential) {
        stored = OSAllocatedUnfairLock(initialState: credential)
    }

    func load(_ id: UUID) throws -> AccountCredential? { stored.withLock { $0 } }
    func save(_ credential: AccountCredential, for id: UUID) throws { stored.withLock { $0 = credential } }
    func update(_ credential: AccountCredential, for id: UUID) throws { stored.withLock { $0 = credential } }
    func delete(_ id: UUID) throws { stored.withLock { $0 = nil } }
}

enum CredentialVaultError: LocalizedError {
    case missingCredential

    var errorDescription: String? {
        "This account's sign-in is missing from the keyring. Remove the account and add it again with `openusage add`."
    }
}

/// Named for the Mac vault it stands in for, so the shared code that defaults to it compiles unchanged.
struct KeychainCredentialVault: CredentialVault {
    static let service = "com.robinebers.openusage.accounts"
    var tool = SecretTool()

    func load(_ id: UUID) throws -> AccountCredential? {
        guard let data = try tool.lookup(service: Self.service, account: id.uuidString) else { return nil }
        return try JSONDecoder().decode(AccountCredential.self, from: data)
    }

    func save(_ credential: AccountCredential, for id: UUID) throws {
        try tool.store(try JSONEncoder().encode(credential), service: Self.service, account: id.uuidString)
    }

    func update(_ credential: AccountCredential, for id: UUID) throws {
        guard try tool.lookup(service: Self.service, account: id.uuidString) != nil else {
            throw CredentialVaultError.missingCredential
        }
        try save(credential, for: id)
    }

    func delete(_ id: UUID) throws {
        try tool.clear(service: Self.service, account: id.uuidString)
    }
}

enum KeychainVaultError: LocalizedError {
    case toolMissing
    case failed(String)

    var errorDescription: String? {
        switch self {
        case .toolMissing:
            "OpenUsage needs `secret-tool` (libsecret) and a running keyring such as GNOME Keyring to store sign-ins."
        case .failed(let reason):
            "OpenUsage couldn't use the keyring for this account (\(reason))."
        }
    }
}

/// Runs libsecret's `secret-tool`. Secrets travel over stdin and stdout only, never as arguments.
struct SecretTool: Sendable {
    var executable = "secret-tool"

    func lookup(service: String, account: String) throws -> Data? {
        let result = try run(["lookup", "service", service, "account", account], input: nil)
        if result.status == 0 { return result.output }
        // `secret-tool lookup` exits 1 without a message when nothing matches.
        if result.status == 1, result.error.isEmpty { return nil }
        throw KeychainVaultError.failed(result.errorSummary)
    }

    func store(_ secret: Data, service: String, account: String) throws {
        let label = "OpenUsage account \(account)"
        let result = try run(["store", "--label=\(label)", "service", service, "account", account], input: secret)
        guard result.status == 0 else { throw KeychainVaultError.failed(result.errorSummary) }
    }

    func clear(service: String, account: String) throws {
        let result = try run(["clear", "service", service, "account", account], input: nil)
        // `clear` exits 1 when nothing matched, which leaves the account as deleted as asked.
        guard result.status == 0 || (result.status == 1 && result.error.isEmpty) else {
            throw KeychainVaultError.failed(result.errorSummary)
        }
    }

    private struct Result {
        var status: Int32
        var output: Data
        var error: String

        var errorSummary: String {
            let line = error.split(separator: "\n").first.map(String.init) ?? ""
            return line.isEmpty ? "secret-tool exited \(status)" : line
        }
    }

    private func run(_ arguments: [String], input: Data?) throws -> Result {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/env")
        process.arguments = [executable] + arguments
        let stdin = Pipe(), stdout = Pipe(), stderr = Pipe()
        process.standardInput = stdin
        process.standardOutput = stdout
        process.standardError = stderr
        do {
            try process.run()
        } catch {
            throw KeychainVaultError.toolMissing
        }
        if let input { stdin.fileHandleForWriting.write(input) }
        try? stdin.fileHandleForWriting.close()
        // Read both pipes before waiting, so a full pipe never stalls the tool.
        let output = stdout.fileHandleForReading.readDataToEndOfFile()
        let error = stderr.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        if process.terminationStatus == 127 { throw KeychainVaultError.toolMissing }
        return Result(
            status: process.terminationStatus,
            output: output,
            error: String(decoding: error, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
        )
    }
}
