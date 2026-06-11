import CScpCore
import Foundation
import UniformTypeIdentifiers

enum Proto: Int32, Codable, CaseIterable {
    case sftp = 0
    case ftp = 1
    case ftps = 2
    case s3 = 3

    var label: String {
        switch self {
        case .sftp: return "SFTP"
        case .ftp: return "FTP"
        case .ftps: return "FTPS"
        case .s3: return "S3"
        }
    }
}

/// One row in a directory listing (local or remote).
struct FileEntry: Identifiable, Hashable {
    /// Stable across refreshes (names are unique within a directory) so the
    /// selection survives a relist — a fresh UUID per listing wiped it.
    var id: String { name }
    var name: String
    var isDir: Bool
    var size: UInt64
    var mtime: Date? = nil
    var perms: String?
    var isSymlink: Bool = false
    /// Owner/group numeric IDs (SFTP only; nil for FTP/S3).
    var uid: UInt32? = nil
    var gid: UInt32? = nil

    /// WinSCP-style "Type" column, from the system's type database.
    var typeDescription: String {
        if isDir { return "File folder" }
        let ext = (name as NSString).pathExtension
        if ext.isEmpty { return "File" }
        if let ut = UTType(filenameExtension: ext.lowercased()),
            let desc = ut.localizedDescription
        {
            return desc
        }
        return "\(ext.uppercased()) file"
    }

    /// Parse "rwxr-xr-x" into permission bits (0o755), if present.
    var mode: UInt32? {
        guard let perms, perms.count == 9 else { return nil }
        var mode: UInt32 = 0
        for (i, c) in perms.enumerated() {
            if c != "-" { mode |= 1 << (8 - i) }
        }
        return mode
    }
}

struct CoreError: LocalizedError {
    let message: String
    var code: Int32 = 1  // SCP_ERR_GENERIC
    var fingerprint: String?
    var errorDescription: String? { message }

    var isUnknownHostKey: Bool { code == SCP_ERR_UNKNOWN_HOST_KEY }
    var isHostKeyMismatch: Bool { code == SCP_ERR_HOST_KEY_MISMATCH }
}

enum HostKeyMode: Int32 {
    case strict = 0
    case acceptNew = 1
    case acceptFingerprint = 2
}

enum AuthMode: Int32, Codable, CaseIterable {
    case password = 0
    case keyFile = 1
    case agent = 2

    var label: String {
        switch self {
        case .password: return "Password"
        case .keyFile: return "Key file"
        case .agent: return "SSH agent"
        }
    }
}

/// Thread-safe cancellation flag shared between the UI and a worker callback.
final class CancelFlag: @unchecked Sendable {
    private let lock = NSLock()
    private var value = false

    func cancel() {
        lock.lock()
        value = true
        lock.unlock()
    }

    var isCancelled: Bool {
        lock.lock()
        defer { lock.unlock() }
        return value
    }
}

/// Heap box so a Swift progress closure can ride through C `user_data`.
/// Returning `false` cancels the transfer.
private final class ProgressBox {
    let onProgress: (UInt64, UInt64) -> Bool
    init(_ f: @escaping (UInt64, UInt64) -> Bool) { self.onProgress = f }
}

/// Same, for multi-file operations: `(kind, fileName, done, total) -> keepGoing`.
/// kind 0 = starting fileName, 1 = byte progress, 2 = file finished.
private final class XferBox {
    let onEvent: (Int32, String?, UInt64, UInt64) -> Bool
    init(_ f: @escaping (Int32, String?, UInt64, UInt64) -> Bool) { self.onEvent = f }
}

/// Thin Swift wrapper over the C ABI in scp_core.h.
///
/// All calls are synchronous (the core is blocking), so callers should hop off
/// the main thread for connect/list/transfer and publish results back.
/// `@unchecked Sendable`: the single `session` pointer is only ever touched
/// from the serial core dispatch queue, never concurrently.
final class CoreClient: @unchecked Sendable {
    private var session: OpaquePointer?

    var isConnected: Bool { session != nil }

    /// Empty strings for bucket/region/fingerprint/keyPath mean "absent" (the
    /// FFI treats them as null; Swift can't pass nullable C strings directly).
    /// In `.keyFile` mode, `password` is the key's passphrase.
    func connect(
        proto: Proto,
        host: String,
        port: UInt16,
        user: String,
        password: String,
        bucket: String = "",
        region: String = "",
        hostKeyMode: HostKeyMode = .strict,
        trustedFingerprint: String = "",
        authMode: AuthMode = .password,
        keyPath: String = ""
    ) throws {
        disconnect()
        let handle = scp_connect(
            proto.rawValue, host, port, user, password,
            bucket, region, hostKeyMode.rawValue, trustedFingerprint,
            authMode.rawValue, keyPath)
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
    func download(remote: String, local: String, onProgress: @escaping (UInt64, UInt64) -> Bool)
        throws -> Int64
    {
        guard let session else { throw CoreError(message: "not connected") }
        let box = ProgressBox(onProgress)
        let ud = Unmanaged.passRetained(box).toOpaque()
        defer { Unmanaged<ProgressBox>.fromOpaque(ud).release() }
        let n = scp_download_cb(session, remote, local, Self.trampoline, ud)
        if n < 0 { throw Self.lastError() }
        return n
    }

    @discardableResult
    func upload(local: String, remote: String, onProgress: @escaping (UInt64, UInt64) -> Bool)
        throws -> Int64
    {
        guard let session else { throw CoreError(message: "not connected") }
        let box = ProgressBox(onProgress)
        let ud = Unmanaged.passRetained(box).toOpaque()
        defer { Unmanaged<ProgressBox>.fromOpaque(ud).release() }
        let n = scp_upload_cb(session, local, remote, Self.trampoline, ud)
        if n < 0 { throw Self.lastError() }
        return n
    }

    @discardableResult
    func downloadDir(
        remote: String, local: String, excludes: String = "",
        onEvent: @escaping (Int32, String?, UInt64, UInt64) -> Bool
    ) throws -> Int64 {
        guard let session else { throw CoreError(message: "not connected") }
        let box = XferBox(onEvent)
        let ud = Unmanaged.passRetained(box).toOpaque()
        defer { Unmanaged<XferBox>.fromOpaque(ud).release() }
        let n = scp_download_dir(session, remote, local, excludes, Self.xferTrampoline, ud)
        if n < 0 { throw Self.lastError() }
        return n
    }

    @discardableResult
    func uploadDir(
        local: String, remote: String, excludes: String = "",
        onEvent: @escaping (Int32, String?, UInt64, UInt64) -> Bool
    ) throws -> Int64 {
        guard let session else { throw CoreError(message: "not connected") }
        let box = XferBox(onEvent)
        let ud = Unmanaged.passRetained(box).toOpaque()
        defer { Unmanaged<XferBox>.fromOpaque(ud).release() }
        let n = scp_upload_dir(session, local, remote, excludes, Self.xferTrampoline, ud)
        if n < 0 { throw Self.lastError() }
        return n
    }

    /// Returns the number of files copied.
    @discardableResult
    func syncDir(
        local: String, remote: String, download: Bool, excludes: String = "",
        deleteExtraneous: Bool = false,
        onEvent: @escaping (Int32, String?, UInt64, UInt64) -> Bool
    ) throws -> Int64 {
        guard let session else { throw CoreError(message: "not connected") }
        let box = XferBox(onEvent)
        let ud = Unmanaged.passRetained(box).toOpaque()
        defer { Unmanaged<XferBox>.fromOpaque(ud).release() }
        let n = scp_sync_dir(
            session, local, remote, download ? 1 : 0, excludes,
            deleteExtraneous ? 1 : 0, Self.xferTrampoline, ud)
        if n < 0 { throw Self.lastError() }
        return n
    }

    /// Sync dry run: what would copy (and delete in mirror mode), without copying.
    struct SyncPlan: Codable {
        var dirs: [String]
        var items: [SyncPlanItem]
        var deletes: [String] = []
    }

    struct SyncPlanItem: Codable, Identifiable {
        var rel: String
        var size: UInt64
        var reason: String
        var id: String { rel }
    }

    func syncPlan(
        local: String, remote: String, download: Bool, excludes: String = "",
        deleteExtraneous: Bool = false
    ) throws -> SyncPlan {
        guard let session else { throw CoreError(message: "not connected") }
        guard let raw = scp_sync_plan(
            session, local, remote, download ? 1 : 0, excludes,
            deleteExtraneous ? 1 : 0)
        else { throw Self.lastError() }
        defer { scp_string_free(raw) }
        return try JSONDecoder().decode(SyncPlan.self, from: Data(String(cString: raw).utf8))
    }

    /// Execute a remote command (SFTP/SSH sessions only).
    struct ExecResult {
        var exitCode: Int32
        var stdout: String
        var stderr: String
    }

    private struct ExecWire: Decodable {
        let exit_code: Int32
        let stdout: String
        let stderr: String
    }

    func execCommand(_ cmd: String) throws -> ExecResult {
        guard let session else { throw CoreError(message: "not connected") }
        guard let raw = scp_exec_command(session, cmd) else { throw Self.lastError() }
        defer { scp_string_free(raw) }
        let wire = try JSONDecoder().decode(ExecWire.self, from: Data(String(cString: raw).utf8))
        return ExecResult(exitCode: wire.exit_code, stdout: wire.stdout, stderr: wire.stderr)
    }

    /// Server-side file copy (same session). Returns bytes copied.
    @discardableResult
    func copyFile(src: String, dst: String) throws -> Int64 {
        guard let session else { throw CoreError(message: "not connected") }
        let n = scp_copy_file(session, src, dst)
        if n < 0 { throw Self.lastError() }
        return n
    }

    struct FindHit: Codable, Identifiable {
        var path: String
        var is_dir: Bool
        var size: UInt64
        var id: String { path }
    }

    /// Recursive remote search by mask (e.g. "*.log"), capped at `limit`.
    func find(base: String, mask: String, limit: UInt32 = 500) throws -> [FindHit] {
        guard let session else { throw CoreError(message: "not connected") }
        guard let raw = scp_find(session, base, mask, limit) else { throw Self.lastError() }
        defer { scp_string_free(raw) }
        return try JSONDecoder().decode([FindHit].self, from: Data(String(cString: raw).utf8))
    }

    /// Resume an upload (appends after the remote file's current size).
    @discardableResult
    func uploadResume(
        local: String, remote: String,
        onProgress: @escaping (UInt64, UInt64) -> Bool
    ) throws -> Int64 {
        guard let session else { throw CoreError(message: "not connected") }
        let box = ProgressBox(onProgress)
        let ud = Unmanaged.passRetained(box).toOpaque()
        defer { Unmanaged<ProgressBox>.fromOpaque(ud).release() }
        let n = scp_upload_resume_cb(session, local, remote, Self.trampoline, ud)
        if n < 0 { throw Self.lastError() }
        return n
    }

    func mkdir(_ path: String) throws {
        guard let session else { throw CoreError(message: "not connected") }
        if scp_mkdir(session, path) != 0 { throw Self.lastError() }
    }

    func removeFile(_ path: String) throws {
        guard let session else { throw CoreError(message: "not connected") }
        if scp_remove_file(session, path) != 0 { throw Self.lastError() }
    }

    func removeDirAll(_ path: String) throws {
        guard let session else { throw CoreError(message: "not connected") }
        if scp_remove_dir_all(session, path) != 0 { throw Self.lastError() }
    }

    func rename(from: String, to: String) throws {
        guard let session else { throw CoreError(message: "not connected") }
        if scp_rename(session, from, to) != 0 { throw Self.lastError() }
    }

    func chmod(_ path: String, mode: UInt32) throws {
        guard let session else { throw CoreError(message: "not connected") }
        if scp_chmod(session, path, mode) != 0 { throw Self.lastError() }
    }

    /// Resume a download from `offset` (appends to the local partial file).
    @discardableResult
    func downloadResume(
        remote: String, local: String, offset: UInt64,
        onProgress: @escaping (UInt64, UInt64) -> Bool
    ) throws -> Int64 {
        guard let session else { throw CoreError(message: "not connected") }
        let box = ProgressBox(onProgress)
        let ud = Unmanaged.passRetained(box).toOpaque()
        defer { Unmanaged<ProgressBox>.fromOpaque(ud).release() }
        let n = scp_download_resume_cb(session, remote, local, offset, Self.trampoline, ud)
        if n < 0 { throw Self.lastError() }
        return n
    }

    /// Liveness probe / NAT keepalive; failures are fine (the core
    /// auto-reconnects on the next real operation).
    func keepalive() {
        guard let session else { return }
        _ = scp_keepalive(session)
    }

    func disconnect() {
        if let session {
            scp_disconnect_free(session)
            self.session = nil
        }
    }

    deinit { disconnect() }

    // MARK: - Helpers

    /// Non-capturing C trampolines: recover the Swift closure from `user_data`.
    /// Return 0 to continue, 1 to cancel.
    private static let trampoline: ScpProgressCb = { transferred, total, user in
        guard let user else { return 0 }
        let box = Unmanaged<ProgressBox>.fromOpaque(user).takeUnretainedValue()
        return box.onProgress(transferred, total) ? 0 : 1
    }

    private static let xferTrampoline: ScpXferCb = { kind, file, done, total, user in
        guard let user else { return 0 }
        let box = Unmanaged<XferBox>.fromOpaque(user).takeUnretainedValue()
        let name = file.map { String(cString: $0) }
        return box.onEvent(kind, name, done, total) ? 0 : 1
    }

    private static func lastError() -> CoreError {
        let message = scp_last_error().map { String(cString: $0) } ?? "unknown error"
        let fingerprint = scp_last_fingerprint().map { String(cString: $0) }
        return CoreError(
            message: message, code: scp_last_error_code(), fingerprint: fingerprint)
    }

    private struct Wire: Decodable {
        let name: String
        let is_dir: Bool
        let size: UInt64
        let mtime: Int64?
        let perms: String?
        let is_symlink: Bool?
        let uid: UInt32?
        let gid: UInt32?
    }

    private static func decode(_ json: String) throws -> [FileEntry] {
        let data = Data(json.utf8)
        let wire = try JSONDecoder().decode([Wire].self, from: data)
        return wire.map {
            FileEntry(
                name: $0.name, isDir: $0.is_dir, size: $0.size,
                mtime: $0.mtime.map { Date(timeIntervalSince1970: TimeInterval($0)) },
                perms: $0.perms,
                isSymlink: $0.is_symlink ?? false,
                uid: $0.uid,
                gid: $0.gid)
        }
    }
}
