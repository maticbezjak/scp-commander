import AppKit
import Foundation
import SwiftUI

/// Observable state for the whole window: connection fields, both pane
/// listings, the transfer queue, edit-in-editor sessions, and status.
/// Blocking core calls run on a serial worker queue off the main thread.
@MainActor
final class AppState: ObservableObject {
    // Connection form
    @Published var proto: Proto = .sftp
    @Published var host = ""
    @Published var port = "22"
    @Published var user = ""
    @Published var password = ""
    @Published var authMode: AuthMode = .password
    @Published var keyPath = ""
    // S3 only
    @Published var bucket = ""
    @Published var region = ""

    /// Fingerprint of an unknown server key awaiting the user's trust decision.
    @Published var hostKeyPrompt: String?

    // Panes
    @Published var localPath = FileManager.default.homeDirectoryForCurrentUser.path
    @Published var localEntries: [FileEntry] = []
    @Published var remotePath = "/"
    @Published var remoteEntries: [FileEntry] = []

    @Published var status = "Not connected"
    @Published var busy = false

    let transfers = TransferQueue()
    let sites = SitesStore()

    private let client = CoreClient()
    private let queue = DispatchQueue(label: "net.manto.ScpCommander.core")

    // Edit-in-editor sessions: remote file -> local temp copy, re-uploaded on save.
    private struct EditSession {
        let remote: String
        let local: URL
        var lastModified: Date
    }
    private var edits: [EditSession] = []
    private var editTimer: Timer?

    var isConnected: Bool { client.isConnected }

    init() {
        loadLocal()
    }

    // MARK: - Saved sites

    func saveCurrentSite() {
        let name = host.isEmpty ? "New site" : "\(user.isEmpty ? "" : "\(user)@")\(host)"
        sites.add(Site(name: name, proto: proto, host: host, port: port, user: user))
        if !password.isEmpty && authMode == .password {
            Keychain.save(
                account: Keychain.account(proto: proto, user: user, host: host, port: port),
                password: password)
            status = "Saved site “\(name)” (password in Keychain)"
        } else {
            status = "Saved site “\(name)”"
        }
    }

    func loadSite(_ site: Site) {
        proto = site.proto
        host = site.host
        port = site.port
        user = site.user
        let account = Keychain.account(
            proto: site.proto, user: site.user, host: site.host, port: site.port)
        if let stored = Keychain.load(account: account) {
            password = stored
            status = "Loaded “\(site.name)” — password from Keychain"
        } else {
            password = ""
            status = "Loaded “\(site.name)” — enter password and Connect"
        }
    }

    func removeSite(_ site: Site) {
        Keychain.delete(
            account: Keychain.account(
                proto: site.proto, user: site.user, host: site.host, port: site.port))
        sites.remove(site)
    }

    // MARK: - Local filesystem

    func loadLocal() {
        let keys: Set<URLResourceKey> = [.isDirectoryKey, .fileSizeKey]
        let url = URL(fileURLWithPath: localPath)
        let contents =
            (try? FileManager.default.contentsOfDirectory(
                at: url, includingPropertiesForKeys: Array(keys))) ?? []
        var entries = contents.map { u -> FileEntry in
            let rv = try? u.resourceValues(forKeys: keys)
            return FileEntry(
                name: u.lastPathComponent,
                isDir: rv?.isDirectory ?? false,
                size: UInt64(rv?.fileSize ?? 0),
                perms: nil)
        }
        sortEntries(&entries)
        localEntries = entries
    }

    func openLocal(_ entry: FileEntry) {
        if entry.isDir {
            localPath = pathJoin(localPath, entry.name)
            loadLocal()
        } else {
            upload(entry)
        }
    }

    func localUp() {
        localPath = (localPath as NSString).deletingLastPathComponent
        if localPath.isEmpty { localPath = "/" }
        loadLocal()
    }

    func newLocalFolder(named name: String) {
        guard !name.isEmpty else { return }
        do {
            try FileManager.default.createDirectory(
                atPath: pathJoin(localPath, name), withIntermediateDirectories: false)
            loadLocal()
        } catch {
            status = "Error: \(error.localizedDescription)"
        }
    }

    func renameLocal(_ entry: FileEntry, to newName: String) {
        guard !newName.isEmpty, newName != entry.name else { return }
        do {
            try FileManager.default.moveItem(
                atPath: pathJoin(localPath, entry.name),
                toPath: pathJoin(localPath, newName))
            loadLocal()
        } catch {
            status = "Error: \(error.localizedDescription)"
        }
    }

    func deleteLocal(_ entry: FileEntry) {
        do {
            try FileManager.default.removeItem(atPath: pathJoin(localPath, entry.name))
            loadLocal()
            status = "Deleted \(entry.name)"
        } catch {
            status = "Error: \(error.localizedDescription)"
        }
    }

    // MARK: - Remote: connect & browse

    /// Connect with the current form values. After an "unknown host key"
    /// failure, the UI re-calls this with the fingerprint the user approved.
    func connect(trustingFingerprint trusted: String? = nil) {
        let p = proto
        let h = host
        let portNum = UInt16(port) ?? Credentials_defaultPort(p)
        let u = user
        let pw = password
        let bkt = bucket
        let rgn = region
        let auth = authMode
        let key = keyPath
        let path = remotePath
        runBusy("Connecting…") { [client] in
            try client.connect(
                proto: p, host: h, port: portNum, user: u, password: pw,
                bucket: bkt, region: rgn,
                hostKeyMode: trusted == nil ? .strict : .acceptFingerprint,
                trustedFingerprint: trusted ?? "",
                authMode: p == .sftp ? auth : .password,
                keyPath: key)
            return try client.listDir(path)
        } onSuccess: { [weak self] entries in
            self?.showRemote(path: path, entries: entries)
            self?.status = "Connected — \(path) (\(entries.count) items)"
        } onFailure: { [weak self] error in
            guard let self else { return }
            if let core = error as? CoreError, core.isUnknownHostKey,
                let fingerprint = core.fingerprint
            {
                self.hostKeyPrompt = fingerprint
                self.status = "Server key not recognized — confirm fingerprint to connect"
            } else {
                self.status = "Error: \(error.localizedDescription)"
            }
        }
    }

    func openRemote(_ entry: FileEntry) {
        if entry.isDir {
            listRemote(pathJoinPosix(remotePath, entry.name))
        } else {
            download(entry)
        }
    }

    func remoteUp() {
        listRemote(parentPosix(remotePath))
    }

    func listRemote(_ path: String) {
        guard isConnected else { return }
        runBusy("Listing \(path)…") { [client] in
            try client.listDir(path)
        } onSuccess: { [weak self] entries in
            self?.showRemote(path: path, entries: entries)
            self?.status = "\(path) (\(entries.count) items)"
        }
    }

    private func showRemote(path: String, entries: [FileEntry]) {
        var sorted = entries
        sortEntries(&sorted)
        remotePath = path
        remoteEntries = sorted
    }

    func refreshRemote() {
        listRemote(remotePath)
    }

    // MARK: - Remote: file management

    func newRemoteFolder(named name: String) {
        guard !name.isEmpty, isConnected else { return }
        let path = pathJoinPosix(remotePath, name)
        runBusy("Creating \(name)…") { [client] in
            try client.mkdir(path)
        } onSuccess: { [weak self] _ in
            self?.refreshRemote()
        }
    }

    func renameRemote(_ entry: FileEntry, to newName: String) {
        guard !newName.isEmpty, newName != entry.name, isConnected else { return }
        let from = pathJoinPosix(remotePath, entry.name)
        let to = pathJoinPosix(remotePath, newName)
        runBusy("Renaming…") { [client] in
            try client.rename(from: from, to: to)
        } onSuccess: { [weak self] _ in
            self?.refreshRemote()
        }
    }

    func deleteRemote(_ entry: FileEntry) {
        guard isConnected else { return }
        let path = pathJoinPosix(remotePath, entry.name)
        let isDir = entry.isDir
        runBusy("Deleting \(entry.name)…") { [client] in
            if isDir {
                try client.removeDirAll(path)
            } else {
                try client.removeFile(path)
            }
        } onSuccess: { [weak self] _ in
            self?.status = "Deleted \(entry.name)"
            self?.refreshRemote()
        }
    }

    // MARK: - Transfers

    func download(_ entry: FileEntry) {
        guard isConnected else { return }
        if entry.isDir {
            downloadFolder(entry)
            return
        }
        transferFile(
            remote: pathJoinPosix(remotePath, entry.name),
            local: pathJoin(localPath, entry.name),
            name: entry.name,
            size: entry.size,
            direction: .download
        ) { [weak self] in self?.loadLocal() }
    }

    func upload(_ entry: FileEntry) {
        guard isConnected else {
            status = "Connect first to upload"
            return
        }
        if entry.isDir {
            uploadFolder(entry)
            return
        }
        transferFile(
            remote: pathJoinPosix(remotePath, entry.name),
            local: pathJoin(localPath, entry.name),
            name: entry.name,
            size: entry.size,
            direction: .upload
        ) { [weak self] in self?.refreshRemote() }
    }

    /// Single-file transfer with a progress row; `onDone` runs after success.
    private func transferFile(
        remote: String, local: String, name: String, size: UInt64,
        direction: TransferDirection, onDone: @escaping () -> Void
    ) {
        let transfer = Transfer(name: name, direction: direction)
        transfer.total = size
        transfers.add(transfer)
        let flag = transfer.cancelFlag

        queue.async { [weak self, client] in
            let progress: (UInt64, UInt64) -> Bool = { done, total in
                DispatchQueue.main.async {
                    transfer.transferred = done
                    if total > 0 { transfer.total = total }
                }
                return !flag.isCancelled
            }
            let result = Result {
                direction == .download
                    ? try client.download(remote: remote, local: local, onProgress: progress)
                    : try client.upload(local: local, remote: remote, onProgress: progress)
            }
            DispatchQueue.main.async {
                switch result {
                case .success:
                    transfer.state = .done
                    self?.status = "\(direction == .download ? "Downloaded" : "Uploaded") \(name)"
                    onDone()
                case .failure where flag.isCancelled:
                    transfer.state = .cancelled
                    self?.status = "Cancelled \(name)"
                case .failure(let error):
                    transfer.state = .failed(error.localizedDescription)
                    self?.status = "Error: \(error.localizedDescription)"
                }
            }
        }
    }

    func downloadFolder(_ entry: FileEntry) {
        guard isConnected, entry.isDir else { return }
        runFolderOp(
            name: entry.name, direction: .download,
            remote: pathJoinPosix(remotePath, entry.name),
            local: pathJoin(localPath, entry.name)
        ) { [weak self] in self?.loadLocal() }
    }

    func uploadFolder(_ entry: FileEntry) {
        guard isConnected, entry.isDir else { return }
        runFolderOp(
            name: entry.name, direction: .upload,
            remote: pathJoinPosix(remotePath, entry.name),
            local: pathJoin(localPath, entry.name)
        ) { [weak self] in self?.refreshRemote() }
    }

    func sync(download: Bool) {
        guard isConnected else {
            status = "Connect first to sync"
            return
        }
        let local = localPath
        let remote = remotePath
        let title = download ? "Sync ⬇ \(remote)" : "Sync ⬆ \(remote)"
        let transfer = Transfer(name: title, direction: download ? .download : .upload)
        transfers.add(transfer)
        let flag = transfer.cancelFlag

        queue.async { [weak self, client] in
            let result = Result {
                try client.syncDir(
                    local: local, remote: remote, download: download,
                    onEvent: Self.folderEventHandler(transfer: transfer, flag: flag))
            }
            DispatchQueue.main.async {
                switch result {
                case .success(let copied):
                    transfer.state = .done
                    self?.status = "Sync done — \(copied) file(s) copied"
                    if download { self?.loadLocal() } else { self?.refreshRemote() }
                case .failure where flag.isCancelled:
                    transfer.state = .cancelled
                    self?.status = "Sync cancelled"
                case .failure(let error):
                    transfer.state = .failed(error.localizedDescription)
                    self?.status = "Error: \(error.localizedDescription)"
                }
            }
        }
    }

    /// Recursive folder transfer with one queue row updated per file.
    private func runFolderOp(
        name: String, direction: TransferDirection, remote: String, local: String,
        onDone: @escaping () -> Void
    ) {
        let transfer = Transfer(name: "\(name)/", direction: direction)
        transfers.add(transfer)
        let flag = transfer.cancelFlag

        queue.async { [weak self, client] in
            let handler = Self.folderEventHandler(transfer: transfer, flag: flag)
            let result = Result {
                direction == .download
                    ? try client.downloadDir(remote: remote, local: local, onEvent: handler)
                    : try client.uploadDir(local: local, remote: remote, onEvent: handler)
            }
            DispatchQueue.main.async {
                switch result {
                case .success(let bytes):
                    transfer.state = .done
                    transfer.transferred = UInt64(max(0, bytes))
                    self?.status = "Folder \(name): \(transfer.filesDone) file(s)"
                    onDone()
                case .failure where flag.isCancelled:
                    transfer.state = .cancelled
                    self?.status = "Cancelled \(name)"
                case .failure(let error):
                    transfer.state = .failed(error.localizedDescription)
                    self?.status = "Error: \(error.localizedDescription)"
                }
            }
        }
    }

    /// Shared multi-file event handler: updates the row, honours cancellation.
    /// Runs on the worker thread; all row updates hop to the main queue.
    private nonisolated static func folderEventHandler(transfer: Transfer, flag: CancelFlag)
        -> (Int32, String?, UInt64, UInt64) -> Bool
    {
        return { kind, file, done, total in
            DispatchQueue.main.async {
                switch kind {
                case 0:
                    transfer.currentFile = file
                    transfer.total = total
                    transfer.transferred = 0
                case 1:
                    transfer.transferred = done
                    if total > 0 { transfer.total = total }
                case 2:
                    transfer.filesDone += 1
                default:
                    break
                }
            }
            return !flag.isCancelled
        }
    }

    // MARK: - Edit in editor

    /// Download to a temp copy, open it in the default app, and re-upload
    /// whenever the file is saved (mtime polling).
    func editRemote(_ entry: FileEntry) {
        guard isConnected, !entry.isDir else { return }
        let remote = pathJoinPosix(remotePath, entry.name)
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("ScpCommander-edit")
            .appendingPathComponent(UUID().uuidString)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let local = dir.appendingPathComponent(entry.name)

        transferFile(
            remote: remote, local: local.path, name: entry.name, size: entry.size,
            direction: .download
        ) { [weak self] in
            guard let self else { return }
            let mtime =
                (try? FileManager.default.attributesOfItem(atPath: local.path)[.modificationDate]
                    as? Date) ?? Date()
            self.edits.append(EditSession(remote: remote, local: local, lastModified: mtime))
            NSWorkspace.shared.open(local)
            self.status = "Editing \(entry.name) — saves upload automatically"
            self.startEditTimerIfNeeded()
        }
    }

    private func startEditTimerIfNeeded() {
        guard editTimer == nil else { return }
        editTimer = Timer.scheduledTimer(withTimeInterval: 2, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.pollEdits() }
        }
    }

    private func pollEdits() {
        for i in edits.indices {
            let session = edits[i]
            guard
                let mtime = (try? FileManager.default.attributesOfItem(
                    atPath: session.local.path))?[.modificationDate] as? Date,
                mtime > session.lastModified
            else { continue }
            edits[i].lastModified = mtime
            let size =
                (try? FileManager.default.attributesOfItem(atPath: session.local.path))?[.size]
                as? UInt64 ?? 0
            transferFile(
                remote: session.remote, local: session.local.path,
                name: session.local.lastPathComponent, size: size,
                direction: .upload
            ) { [weak self] in self?.refreshRemote() }
        }
    }

    // MARK: - Drag-and-drop entry points

    func uploadByName(_ name: String) {
        if let e = localEntries.first(where: { $0.name == name }) { upload(e) }
    }

    func downloadByName(_ name: String) {
        if let e = remoteEntries.first(where: { $0.name == name }) { download(e) }
    }

    // MARK: - Plumbing

    /// Run a blocking core call off-thread, marshalling result/error back to main.
    private func runBusy<T>(
        _ message: String,
        _ work: @escaping () throws -> T,
        onSuccess: @escaping (T) -> Void,
        onFailure: ((Error) -> Void)? = nil
    ) {
        busy = true
        status = message
        queue.async {
            let result = Result(catching: work)
            DispatchQueue.main.async { [weak self] in
                guard let self else { return }
                self.busy = false
                switch result {
                case .success(let value): onSuccess(value)
                case .failure(let error):
                    if let onFailure {
                        onFailure(error)
                    } else {
                        self.status = "Error: \(error.localizedDescription)"
                    }
                }
            }
        }
    }

    private func sortEntries(_ entries: inout [FileEntry]) {
        entries.sort {
            if $0.isDir != $1.isDir { return $0.isDir }
            return $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending
        }
    }
}

// MARK: - Path helpers

private func pathJoin(_ base: String, _ name: String) -> String {
    (base as NSString).appendingPathComponent(name)
}

private func pathJoinPosix(_ base: String, _ name: String) -> String {
    if base.hasSuffix("/") { return base + name }
    return base + "/" + name
}

private func parentPosix(_ path: String) -> String {
    guard path != "/" else { return "/" }
    let trimmed = path.hasSuffix("/") ? String(path.dropLast()) : path
    if let idx = trimmed.lastIndex(of: "/") {
        let parent = String(trimmed[..<idx])
        return parent.isEmpty ? "/" : parent
    }
    return "/"
}

private func Credentials_defaultPort(_ p: Proto) -> UInt16 {
    switch p {
    case .sftp: return 22
    case .ftp, .ftps: return 21
    case .s3: return 443
    }
}
