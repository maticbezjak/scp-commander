import AppKit
import Foundation
import SwiftUI

/// One server session, WinSCP-tab-style. Each tab owns its own connection,
/// serial worker queue, and cached remote listing; `AppState` publishes the
/// active tab's view of the world.
@MainActor
final class SessionHandle: Identifiable {
    let id = UUID()
    let client = CoreClient()
    let queue = DispatchQueue(label: "net.manto.ScpCommander.session")
    var remotePath = "/"
    var remoteEntries: [FileEntry] = []
    var connected = false
    var title = "New Session"
}

/// Observable state for the whole window. Blocking core calls run on the
/// owning session's serial queue off the main thread.
@MainActor
final class AppState: ObservableObject {
    // Connection form (feeds the Login dialog; applies to the active tab)
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

    // Tabs
    private(set) var sessions: [SessionHandle] = [SessionHandle()]
    @Published var tabTitles: [String] = ["New Session"]
    @Published private(set) var activeTab = 0

    // Active tab's view of the panes
    @Published var localPath = FileManager.default.homeDirectoryForCurrentUser.path
    @Published var localEntries: [FileEntry] = []
    @Published var remotePath = "/"
    @Published var remoteEntries: [FileEntry] = []
    @Published var activeConnected = false

    @Published var status = "Not connected"
    @Published var busy = false

    /// WinSCP-style Login dialog; shown at startup and via "New Session".
    @Published var showLogin = true

    let transfers = TransferQueue()
    let sites = SitesStore()

    // Edit-in-editor sessions: remote file -> local temp copy, re-uploaded on
    // save through the session that opened it.
    private struct EditSession {
        let remote: String
        let local: URL
        var lastModified: Date
        let session: SessionHandle
    }
    private var edits: [EditSession] = []
    private var editTimer: Timer?

    var active: SessionHandle { sessions[activeTab] }
    var isConnected: Bool { activeConnected }

    init() {
        loadLocal()
    }

    // MARK: - Tabs

    func selectTab(_ index: Int) {
        guard sessions.indices.contains(index) else { return }
        activeTab = index
        publishActive()
    }

    func newTab() {
        sessions.append(SessionHandle())
        tabTitles.append("New Session")
        activeTab = sessions.count - 1
        publishActive()
        showLogin = true
    }

    func closeTab(_ index: Int) {
        guard sessions.indices.contains(index) else { return }
        let session = sessions[index]
        session.queue.async { [client = session.client] in client.disconnect() }
        sessions.remove(at: index)
        tabTitles.remove(at: index)
        if sessions.isEmpty {
            sessions = [SessionHandle()]
            tabTitles = ["New Session"]
        }
        activeTab = min(activeTab, sessions.count - 1)
        publishActive()
    }

    /// Mirror the active session's cached state into the published fields.
    private func publishActive() {
        remotePath = active.remotePath
        remoteEntries = active.remoteEntries
        activeConnected = active.connected
    }

    /// Store a listing on its session; update the UI only for the active tab.
    private func showRemote(_ session: SessionHandle, path: String, entries: [FileEntry]) {
        var sorted = entries
        sortEntries(&sorted)
        session.remotePath = path
        session.remoteEntries = sorted
        if session === active {
            remotePath = path
            remoteEntries = sorted
        }
    }

    // MARK: - Saved sites (WinSCP-style)

    @Published var saveSitePrompt = false
    @Published var saveSiteName = ""
    @Published var saveSitePassword = false

    /// Open the save dialog, defaulting the name like WinSCP (user@host).
    /// Use "Folder/Name" as the site name to group sites into folders.
    func beginSaveSite() {
        saveSiteName = host.isEmpty ? "New site" : "\(user.isEmpty ? "" : "\(user)@")\(host)"
        saveSitePassword = false
        saveSitePrompt = true
    }

    func confirmSaveSite() {
        let name = saveSiteName.trimmingCharacters(in: .whitespaces)
        guard !name.isEmpty else { return }
        let site = Site(
            name: name, proto: proto, host: host, port: port, user: user,
            authMode: proto == .sftp ? authMode : .password,
            keyPath: keyPath, bucket: bucket, region: region)
        sites.add(site)
        if saveSitePassword && !password.isEmpty && site.authMode == .password {
            Keychain.save(account: site.keychainAccount, password: password)
            status = "Saved site “\(name)” (password in Keychain)"
        } else {
            status = "Saved site “\(name)”"
        }
        saveSitePrompt = false
    }

    /// Edit: fill the connection form from a site (single click in the list).
    func loadSite(_ site: Site) {
        proto = site.proto
        host = site.host
        port = site.port
        user = site.user
        authMode = site.authMode
        keyPath = site.keyPath
        bucket = site.bucket
        region = site.region
        if site.authMode == .password, let stored = Keychain.load(account: site.keychainAccount) {
            password = stored
            status = "Loaded “\(site.name)” — password from Keychain"
        } else {
            password = ""
            status = site.authMode == .password
                ? "Loaded “\(site.name)” — enter password and Connect"
                : "Loaded “\(site.name)”"
        }
    }

    /// Login: load the site and connect immediately (double click / menu).
    func login(_ site: Site) {
        loadSite(site)
        if site.authMode == .password && password.isEmpty && site.proto != .ftp {
            status = "“\(site.name)” has no stored password — enter it and Connect"
            return
        }
        connect()
    }

    func renameSite(_ site: Site, to newName: String) {
        sites.rename(site, to: newName)
    }

    func removeSite(_ site: Site) {
        Keychain.delete(account: site.keychainAccount)
        sites.remove(site)
    }

    // MARK: - Local filesystem

    func loadLocal() {
        let keys: Set<URLResourceKey> = [.isDirectoryKey, .fileSizeKey, .contentModificationDateKey]
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
                mtime: rv?.contentModificationDate,
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

    func chmodLocal(_ entry: FileEntry, mode: UInt32) {
        let path = pathJoin(localPath, entry.name)
        do {
            try FileManager.default.setAttributes(
                [.posixPermissions: NSNumber(value: mode)], ofItemAtPath: path)
            status = "Permissions of \(entry.name) set to \(String(mode, radix: 8))"
            loadLocal()
        } catch {
            status = "Error: \(error.localizedDescription)"
        }
    }

    /// Current unix mode of a local entry (for the Properties dialog).
    func localMode(of entry: FileEntry) -> UInt32? {
        let path = pathJoin(localPath, entry.name)
        let attrs = try? FileManager.default.attributesOfItem(atPath: path)
        return (attrs?[.posixPermissions] as? NSNumber).map { UInt32(truncating: $0) & 0o777 }
    }

    // MARK: - Remote: connect & browse

    /// Connect the active tab with the current form values. After an "unknown
    /// host key" failure, the UI re-calls this with the approved fingerprint.
    func connect(trustingFingerprint trusted: String? = nil) {
        let session = active
        let p = proto
        let h = host
        let portNum = UInt16(port) ?? Credentials_defaultPort(p)
        let u = user
        let pw = password
        let bkt = bucket
        let rgn = region
        let auth = authMode
        let key = keyPath
        let path = session.remotePath
        runBusy(on: session, "Connecting…") { [client = session.client] in
            try client.connect(
                proto: p, host: h, port: portNum, user: u, password: pw,
                bucket: bkt, region: rgn,
                hostKeyMode: trusted == nil ? .strict : .acceptFingerprint,
                trustedFingerprint: trusted ?? "",
                authMode: p == .sftp ? auth : .password,
                keyPath: key)
            return try client.listDir(path)
        } onSuccess: { [weak self] entries in
            guard let self else { return }
            session.connected = true
            let target = h.isEmpty ? bkt : h
            session.title = u.isEmpty ? target : "\(u)@\(target)"
            if let idx = self.sessions.firstIndex(where: { $0 === session }) {
                self.tabTitles[idx] = session.title
            }
            self.showRemote(session, path: path, entries: entries)
            if session === self.active { self.activeConnected = true }
            self.status = "Connected — \(path) (\(entries.count) items)"
            self.showLogin = false
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
        let session = active
        guard session.connected else { return }
        runBusy(on: session, "Listing \(path)…") { [client = session.client] in
            try client.listDir(path)
        } onSuccess: { [weak self] entries in
            self?.showRemote(session, path: path, entries: entries)
            self?.status = "\(path) (\(entries.count) items)"
        }
    }

    func refreshRemote() {
        listRemote(remotePath)
    }

    /// Refresh a specific session's current directory (post-transfer).
    private func refreshSession(_ session: SessionHandle) {
        guard session.connected else { return }
        let path = session.remotePath
        runBusy(on: session, "Listing \(path)…") { [client = session.client] in
            try client.listDir(path)
        } onSuccess: { [weak self] entries in
            self?.showRemote(session, path: path, entries: entries)
        }
    }

    // MARK: - Remote: file management

    func newRemoteFolder(named name: String) {
        let session = active
        guard !name.isEmpty, session.connected else { return }
        let path = pathJoinPosix(session.remotePath, name)
        runBusy(on: session, "Creating \(name)…") { [client = session.client] in
            try client.mkdir(path)
        } onSuccess: { [weak self] _ in
            self?.refreshSession(session)
        }
    }

    func renameRemote(_ entry: FileEntry, to newName: String) {
        let session = active
        guard !newName.isEmpty, newName != entry.name, session.connected else { return }
        let from = pathJoinPosix(session.remotePath, entry.name)
        let to = pathJoinPosix(session.remotePath, newName)
        runBusy(on: session, "Renaming…") { [client = session.client] in
            try client.rename(from: from, to: to)
        } onSuccess: { [weak self] _ in
            self?.refreshSession(session)
        }
    }

    func deleteRemote(_ entry: FileEntry) {
        let session = active
        guard session.connected else { return }
        let path = pathJoinPosix(session.remotePath, entry.name)
        let isDir = entry.isDir
        runBusy(on: session, "Deleting \(entry.name)…") { [client = session.client] in
            if isDir {
                try client.removeDirAll(path)
            } else {
                try client.removeFile(path)
            }
        } onSuccess: { [weak self] _ in
            self?.status = "Deleted \(entry.name)"
            self?.refreshSession(session)
        }
    }

    func chmodRemote(_ entry: FileEntry, mode: UInt32) {
        let session = active
        guard session.connected else { return }
        let path = pathJoinPosix(session.remotePath, entry.name)
        runBusy(on: session, "Changing permissions…") { [client = session.client] in
            try client.chmod(path, mode: mode)
        } onSuccess: { [weak self] _ in
            self?.status = "Permissions of \(entry.name) set to \(String(mode, radix: 8))"
            self?.refreshSession(session)
        }
    }

    // MARK: - Transfers

    func download(_ entry: FileEntry) {
        let session = active
        guard session.connected else { return }
        if entry.isDir {
            downloadFolder(entry)
            return
        }
        transferFile(
            on: session,
            remote: pathJoinPosix(session.remotePath, entry.name),
            local: pathJoin(localPath, entry.name),
            name: entry.name,
            size: entry.size,
            direction: .download
        ) { [weak self] in self?.loadLocal() }
    }

    func upload(_ entry: FileEntry) {
        let session = active
        guard session.connected else {
            status = "Connect first to upload"
            return
        }
        if entry.isDir {
            uploadFolder(entry)
            return
        }
        transferFile(
            on: session,
            remote: pathJoinPosix(session.remotePath, entry.name),
            local: pathJoin(localPath, entry.name),
            name: entry.name,
            size: entry.size,
            direction: .upload
        ) { [weak self] in self?.refreshSession(session) }
    }

    /// Single-file transfer with a progress row; `onDone` runs after success.
    private func transferFile(
        on session: SessionHandle,
        remote: String, local: String, name: String, size: UInt64,
        direction: TransferDirection, onDone: @escaping () -> Void
    ) {
        let transfer = Transfer(name: name, direction: direction)
        transfer.total = size
        transfers.add(transfer)
        let flag = transfer.cancelFlag

        session.queue.async { [weak self, client = session.client] in
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
        let session = active
        guard session.connected, entry.isDir else { return }
        runFolderOp(
            on: session,
            name: entry.name, direction: .download,
            remote: pathJoinPosix(session.remotePath, entry.name),
            local: pathJoin(localPath, entry.name)
        ) { [weak self] in self?.loadLocal() }
    }

    func uploadFolder(_ entry: FileEntry) {
        let session = active
        guard session.connected, entry.isDir else { return }
        runFolderOp(
            on: session,
            name: entry.name, direction: .upload,
            remote: pathJoinPosix(session.remotePath, entry.name),
            local: pathJoin(localPath, entry.name)
        ) { [weak self] in self?.refreshSession(session) }
    }

    func sync(download: Bool) {
        let session = active
        guard session.connected else {
            status = "Connect first to sync"
            return
        }
        let local = localPath
        let remote = session.remotePath
        let title = download ? "Sync ⬇ \(remote)" : "Sync ⬆ \(remote)"
        let transfer = Transfer(name: title, direction: download ? .download : .upload)
        transfers.add(transfer)
        let flag = transfer.cancelFlag

        session.queue.async { [weak self, client = session.client] in
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
                    if download {
                        self?.loadLocal()
                    } else {
                        self?.refreshSession(session)
                    }
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
        on session: SessionHandle,
        name: String, direction: TransferDirection, remote: String, local: String,
        onDone: @escaping () -> Void
    ) {
        let transfer = Transfer(name: "\(name)/", direction: direction)
        transfers.add(transfer)
        let flag = transfer.cancelFlag

        session.queue.async { [weak self, client = session.client] in
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
    /// whenever the file is saved (mtime polling), via the owning session.
    func editRemote(_ entry: FileEntry) {
        let session = active
        guard session.connected, !entry.isDir else { return }
        let remote = pathJoinPosix(session.remotePath, entry.name)
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("ScpCommander-edit")
            .appendingPathComponent(UUID().uuidString)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let local = dir.appendingPathComponent(entry.name)

        transferFile(
            on: session,
            remote: remote, local: local.path, name: entry.name, size: entry.size,
            direction: .download
        ) { [weak self] in
            guard let self else { return }
            let mtime =
                (try? FileManager.default.attributesOfItem(atPath: local.path)[.modificationDate]
                    as? Date) ?? Date()
            self.edits.append(
                EditSession(remote: remote, local: local, lastModified: mtime, session: session))
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
            let editSession = edits[i]
            guard
                let mtime = (try? FileManager.default.attributesOfItem(
                    atPath: editSession.local.path))?[.modificationDate] as? Date,
                mtime > editSession.lastModified
            else { continue }
            edits[i].lastModified = mtime
            let size =
                (try? FileManager.default.attributesOfItem(atPath: editSession.local.path))?[
                    .size] as? UInt64 ?? 0
            let session = editSession.session
            transferFile(
                on: session,
                remote: editSession.remote, local: editSession.local.path,
                name: editSession.local.lastPathComponent, size: size,
                direction: .upload
            ) { [weak self] in self?.refreshSession(session) }
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

    /// Run a blocking core call on a session's queue, marshalling the
    /// result/error back to the main thread.
    private func runBusy<T>(
        on session: SessionHandle,
        _ message: String,
        _ work: @escaping () throws -> T,
        onSuccess: @escaping (T) -> Void,
        onFailure: ((Error) -> Void)? = nil
    ) {
        busy = true
        status = message
        session.queue.async {
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
