import CScpCore
import Foundation

enum Proto: Int32 {
    case sftp = 0
    case ftp = 1
    case ftps = 2
    case s3 = 3
}

/// One row in a directory listing (local or remote).
struct FileEntry: Identifiable, Hashable {
    let id = UUID()
    let name: String
    let isDir: Bool
    let size: UInt64
    let perms: String?
}

struct CoreError: LocalizedError {
    let message: String
    var errorDescription: String? { message }
}

/// Thin Swift wrapper over the C ABI in scp_core.h.
///
/// All calls are synchronous (the core is blocking), so callers should hop off
/// the main thread for connect/list/transfer and publish results back.
final class CoreClient {
    private var session: OpaquePointer?

    var isConnected: Bool { session != nil }

    func connect(proto: Proto, host: String, port: UInt16, user: String, password: String) throws {
        disconnect()
        let handle = scp_connect(proto.rawValue, host, port, user, password)
        guard let handle else { throw Self.lastError() }
        session = handle
    }

    func listDir(_ path: String) throws -> [FileEntry] {
        guard let session else { throw CoreError(message: "not connected") }
        guard let raw = scp_list_dir(session, path) else { throw Self.lastError() }
        defer { scp_string_free(raw) }
        let json = String(cString: raw)
        return try Self.decode(json)
    }

    @discardableResult
    func download(remote: String, local: String) throws -> Int64 {
        guard let session else { throw CoreError(message: "not connected") }
        let n = scp_download(session, remote, local)
        if n < 0 { throw Self.lastError() }
        return n
    }

    @discardableResult
    func upload(local: String, remote: String) throws -> Int64 {
        guard let session else { throw CoreError(message: "not connected") }
        let n = scp_upload(session, local, remote)
        if n < 0 { throw Self.lastError() }
        return n
    }

    func disconnect() {
        if let session {
            scp_disconnect_free(session)
            self.session = nil
        }
    }

    deinit { disconnect() }

    // MARK: - Helpers

    private static func lastError() -> CoreError {
        if let p = scp_last_error() {
            return CoreError(message: String(cString: p))
        }
        return CoreError(message: "unknown error")
    }

    private struct Wire: Decodable {
        let name: String
        let is_dir: Bool
        let size: UInt64
        let mtime: Int64?
        let perms: String?
    }

    private static func decode(_ json: String) throws -> [FileEntry] {
        let data = Data(json.utf8)
        let wire = try JSONDecoder().decode([Wire].self, from: data)
        return wire.map {
            FileEntry(name: $0.name, isDir: $0.is_dir, size: $0.size, perms: $0.perms)
        }
    }
}
