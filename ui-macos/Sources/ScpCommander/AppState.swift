import AppKit
import Foundation
import SwiftUI

/// One server session, WinSCP-tab-style. Each tab owns its own connection,
/// serial worker queue, and cached remote listing; `AppState` publishes the
/// active tab's view of the world.
@MainActor
final class SessionHandle: Identifiable {
    let id = UUID()
    /// Browse connection: listings and file management.
    let client = CoreClient()
    let queue = DispatchQueue(label: "net.manto.ScpCommander.session")
    /// Dedicated transfer connection so a long download never blocks
    /// browsing (WinSCP's background-transfer model).
    let transferClient = CoreClient()
    let transferQueue = DispatchQueue(label: "net.manto.ScpCommander.transfer")
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

    // Commander state: focused pane, per-pane multi-selection, dotfile toggle.
    // Plain var — changing focus must not re-render the full ContentView.
    // localFocused drives only the pane header highlight.
    var focusedPane: PaneKind = .local {
        didSet { localFocused = focusedPane == .local }
    }
    @Published var localFocused: Bool = true
    @Published var localSelection = Set<FileEntry.ID>()
    @Published var remoteSelection = Set<FileEntry.ID>()
    @Published var showHidden = false

    /// WinSCP-style Login dialog; shown at startup and via "New Session".
    @Published var showLogin = true

    /// Exclusion masks for folder transfers and sync ("*.tmp; .git/").
    @Published var excludeMasks =
        UserDefaults.standard.string(forKey: "excludeMasks") ?? ""
    {
        didSet { UserDefaults.standard.set(excludeMasks, forKey: "excludeMasks") }
    }

    /// Mirror mode: delete destination items that have no source counterpart.
    @Published var mirrorSync =
        UserDefaults.standard.bool(forKey: "mirrorSync")
    {
        didSet { UserDefaults.standard.set(mirrorSync, forKey: "mirrorSync") }
    }

    /// Speed limit in KiB/s (0 = unlimited) — stored but not yet wired into
    /// core callbacks (placeholder for the preferences window).
    @Published var speedLimitKbs: Int =
        UserDefaults.standard.integer(forKey: "speedLimitKbs")
    {
        didSet { UserDefaults.standard.set(speedLimitKbs, forKey: "speedLimitKbs") }
    }

    /// Output of the last Execute Command run.
    @Published var execResult: CoreClient.ExecResult?
    @Published var showExecResult = false
    @Published var showExecDialog = false
    @Published var execCmd = ""

    /// Files awaiting an overwrite decision (destination already exists).
    @Published var overwritePrompt: (pane: PaneKind, entries: [FileEntry])?

    /// Sync dry run awaiting approval in the preview sheet.
    @Published var syncPreview:
        (download: Bool, localRoot: String, remoteRoot: String, plan: CoreClient.SyncPlan)?

    /// Results of the last remote Find.
    @Published var findResults: (mask: String, hits: [CoreClient.FindHit])?
    @Published var showFind = false

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

    private var keepaliveTimer: Timer?

    /// One open tab, persisted for workspace restore. Passwords stay in the
    /// Keychain; restore re-fetches them per site key.
    private struct TabSnapshot: Codable {
        var proto: Proto
        var authMode: AuthMode
        var host: String
        var port: String
        var user: String
        var keyPath: String
        var bucket: String
        var region: String
        var remotePath: String
        var localPath: String
    }

    private var workspaceURL: URL {
        let base =
            FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)
            .first ?? FileManager.default.temporaryDirectory
        return base.appendingPathComponent("ScpCommander/workspace.json")
    }

    /// Persist the connected tabs (settings + paths) for the next launch.
    func saveWorkspace() {
        var snapshots: [TabSnapshot] = []
        for (i, session) in sessions.enumerated() where session.connected {
            // The form fields describe the *active* tab; for others use what
            // we know from the tab title (proto specifics are best-effort).
            if i == activeTab {
                snapshots.append(
                    TabSnapshot(
                        proto: proto, authMode: authMode, host: host, port: port, user: user,
                        keyPath: keyPath, bucket: bucket, region: region,
                        remotePath: session.remotePath, localPath: localPath))
            }
        }
        if let data = try? JSONEncoder().encode(snapshots) {
            try? data.write(to: workspaceURL, options: .atomic)
        }
    }

    /// Restore the saved workspace: prefill + auto-login where a password is
    /// stored (or auth needs none). Returns false when there was nothing.
    private func restoreWorkspace() -> Bool {
        guard let data = try? Data(contentsOf: workspaceURL),
            let snapshots = try? JSONDecoder().decode([TabSnapshot].self, from: data),
            !snapshots.isEmpty
        else { return false }
        let snap = snapshots[0]
        proto = snap.proto
        authMode = snap.authMode
        host = snap.host
        port = snap.port
        user = snap.user
        keyPath = snap.keyPath
        bucket = snap.bucket
        region = snap.region
        active.remotePath = snap.remotePath
        remotePath = snap.remotePath
        if FileManager.default.fileExists(atPath: snap.localPath) {
            localPath = snap.localPath
            loadLocal()
        }
        if snap.authMode == .password {
            let account = Keychain.account(
                proto: snap.proto, user: snap.user, host: snap.host, port: snap.port)
            if let stored = Keychain.load(account: account) {
                password = stored
                connect()
                return true
            }
            status = "Workspace restored — enter password and Connect"
            return true
        }
        connect()
        return true
    }

    init() {
        loadLocal()
        // NAT keepalive every 30s for every connected tab; a session that
        // died anyway is revived by the core on the next operation.
        keepaliveTimer = Timer.scheduledTimer(withTimeInterval: 30, repeats: true) {
            [weak self] _ in
            Task { @MainActor in
                guard let self else { return }
                for session in self.sessions where session.connected {
                    session.queue.async { [client = session.client] in client.keepalive() }
                }
            }
        }
        // Save the workspace on quit; restore last session's tab on launch.
        NotificationCenter.default.addObserver(
            forName: NSApplication.willTerminateNotification, object: nil, queue: .main
        ) { _ in
            Task { @MainActor in AppStateRegistry.shared?.saveWorkspace() }
        }
        AppStateRegistry.shared = self
        if restoreWorkspace() {
            showLogin = false
        }
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
        // Closing a tab LEFT of the active one shifts indices: without this
        // the active pane silently jumps to a different server.
        if index < activeTab { activeTab -= 1 }
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
            keyPath: keyPath, bucket: bucket, region: region,
            remoteDir: active.connected ? active.remotePath : "",
            localDir: active.connected ? localPath : "")
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
        if !site.remoteDir.isEmpty {
            active.remotePath = site.remoteDir
            remotePath = site.remoteDir
        }
        if !site.localDir.isEmpty, FileManager.default.fileExists(atPath: site.localDir) {
            localPath = site.localDir
            loadLocal()
        }
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

    /// Export all sites to a JSON file (no passwords — those stay in the
    /// Keychain). The format is shared with the Ubuntu app.
    func exportSites() {
        let panel = NSSavePanel()
        panel.nameFieldStringValue = "scp-commander-sites.json"
        panel.title = "Export sites"
        guard panel.runModal() == .OK, let url = panel.url else { return }
        do {
            try sites.exportData().write(to: url, options: .atomic)
            status = "Exported \(sites.sites.count) site(s) to \(url.lastPathComponent)"
        } catch {
            status = "Export failed: \(error.localizedDescription)"
        }
    }

    /// Import sessions from a WinSCP.ini file (passwords are not migrated —
    /// WinSCP stores them obfuscated; re-enter and re-save them here).
    func importWinScp() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        panel.title = "Import from WinSCP INI"
        guard panel.runModal() == .OK, let url = panel.url else { return }
        do {
            let text = try String(contentsOf: url, encoding: .utf8)
            let count = try sites.importWinScpIni(text)
            status = "Imported \(count) site(s) from WinSCP (re-enter passwords)"
        } catch {
            status = "Import failed: \(error.localizedDescription)"
        }
    }

    /// Import sites from a JSON export (merges; same-named sites replaced).
    func importSites() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        panel.title = "Import sites"
        guard panel.runModal() == .OK, let url = panel.url else { return }
        do {
            let count = try sites.importData(try Data(contentsOf: url))
            status = "Imported \(count) site(s) from \(url.lastPathComponent)"
        } catch {
            status = "Import failed: \(error.localizedDescription)"
        }
    }

    func removeSite(_ site: Site) {
        Keychain.delete(account: site.keychainAccount)
        sites.remove(site)
    }

    // MARK: - Local filesystem

    func loadLocal() {
        let keys: Set<URLResourceKey> = [
            .isDirectoryKey, .fileSizeKey, .contentModificationDateKey, .isSymbolicLinkKey,
        ]
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
                perms: nil,
                isSymlink: rv?.isSymbolicLink ?? false)
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
            // Bring up the dedicated transfer connection; later transfers
            // queue behind this on the serial transferQueue, so ordering
            // guarantees it's connected first.
            session.transferQueue.async { [client = session.transferClient] in
                try? client.connect(
                    proto: p, host: h, port: portNum, user: u, password: pw,
                    bucket: bkt, region: rgn,
                    hostKeyMode: .strict, trustedFingerprint: "",
                    authMode: p == .sftp ? auth : .password, keyPath: key)
            }
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
        // Resume when the remote copy is a smaller partial of this file.
        let resume = session.remoteEntries.contains {
            !$0.isDir && $0.name == entry.name && $0.size > 0 && $0.size < entry.size
        }
        transferFile(
            on: session,
            remote: pathJoinPosix(session.remotePath, entry.name),
            local: pathJoin(localPath, entry.name),
            name: entry.name,
            size: entry.size,
            direction: .upload,
            resumeUpload: resume
        ) { [weak self] in self?.refreshSession(session) }
    }

    /// Single-file transfer with a progress row; `onDone` runs after success.
    /// Interrupted downloads resume automatically when a smaller partial file
    /// is already present locally.
    private func transferFile(
        on session: SessionHandle,
        remote: String, local: String, name: String, size: UInt64,
        direction: TransferDirection, resumeUpload: Bool = false,
        onDone: @escaping () -> Void
    ) {
        var resumeOffset: UInt64 = 0
        if direction == .download, size > 0,
            let attrs = try? FileManager.default.attributesOfItem(atPath: local),
            let existing = (attrs[.size] as? NSNumber)?.uint64Value,
            existing > 0, existing < size
        {
            resumeOffset = existing
        }

        let transfer = Transfer(name: name, direction: direction)
        transfer.total = size
        transfer.transferred = resumeOffset
        transfers.add(transfer)
        let flag = transfer.cancelFlag
        let offset = resumeOffset

        session.transferQueue.async { [weak self, client = session.transferClient] in
            let progress: (UInt64, UInt64) -> Bool = { done, total in
                DispatchQueue.main.async { transfer.note(done, total: total) }
                return !flag.isCancelled
            }
            let result = Result {
                if direction == .download {
                    if offset > 0 {
                        return try client.downloadResume(
                            remote: remote, local: local, offset: offset, onProgress: progress)
                    }
                    return try client.download(remote: remote, local: local, onProgress: progress)
                }
                if resumeUpload {
                    return try client.uploadResume(
                        local: local, remote: remote, onProgress: progress)
                }
                return try client.upload(local: local, remote: remote, onProgress: progress)
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

    /// Sync is preview-first, WinSCP-style: compute the plan, show the
    /// checklist sheet, copy only what the user approves.
    func sync(download: Bool) {
        let session = active
        guard session.connected else {
            status = "Connect first to sync"
            return
        }
        let local = localPath
        let remote = session.remotePath
        let excludes = excludeMasks
        let mirror = mirrorSync
        status = "Computing sync preview…"
        runBusy(on: session, "Computing sync preview…") { [client = session.client] in
            try client.syncPlan(
                local: local, remote: remote, download: download, excludes: excludes,
                deleteExtraneous: mirror)
        } onSuccess: { [weak self] plan in
            guard let self else { return }
            if plan.items.isEmpty && plan.dirs.isEmpty {
                self.status = "Sync preview: nothing to copy — already in sync"
            } else {
                self.syncPreview = (download, local, remote, plan)
                self.status = "\(plan.items.count) file(s) would copy"
            }
        }
    }

    /// Execute approved items from the sync preview.
    func runSyncItems(_ selected: [CoreClient.SyncPlanItem]) {
        guard let preview = syncPreview else { return }
        syncPreview = nil
        let session = active
        let download = preview.download
        let localRoot = preview.localRoot
        let remoteRoot = preview.remoteRoot

        // Destination directories first.
        if download {
            for dir in preview.plan.dirs {
                try? FileManager.default.createDirectory(
                    atPath: (localRoot as NSString).appendingPathComponent(dir),
                    withIntermediateDirectories: true)
            }
        } else {
            let dirs = preview.plan.dirs
            session.queue.async { [client = session.client] in
                for dir in dirs {
                    try? client.mkdir(remoteRoot + (remoteRoot.hasSuffix("/") ? "" : "/") + dir)
                }
            }
        }
        for item in selected {
            let name = item.rel.split(separator: "/").last.map(String.init) ?? item.rel
            let local = (localRoot as NSString).appendingPathComponent(item.rel)
            let remote = remoteRoot + (remoteRoot.hasSuffix("/") ? "" : "/") + item.rel
            transferFile(
                on: session, remote: remote, local: local, name: name, size: item.size,
                direction: download ? .download : .upload
            ) { [weak self] in
                if download { self?.loadLocal() } else { self?.refreshSession(session) }
            }
        }
        // Mirror-mode: delete destination items that have no source counterpart.
        let deletes = preview.plan.deletes
        if !deletes.isEmpty {
            if download {
                for rel in deletes {
                    let p = (localRoot as NSString).appendingPathComponent(rel)
                    try? FileManager.default.removeItem(atPath: p)
                }
                loadLocal()
            } else {
                session.queue.async { [client = session.client] in
                    for rel in deletes {
                        let p = remoteRoot + (remoteRoot.hasSuffix("/") ? "" : "/") + rel
                        try? client.removeFile(p)
                    }
                }
                refreshSession(session)
            }
        }
        let deleteNote = deletes.isEmpty ? "" : " · deleting \(deletes.count) extraneous"
        status = "Synchronizing \(selected.count) file(s)\(deleteNote)"
    }

    @available(*, deprecated, message: "kept for reference; sync is preview-first now")
    private func syncImmediate(download: Bool) {
        let session = active
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

        let excludes = excludeMasks
        session.transferQueue.async { [weak self, client = session.transferClient] in
            let handler = Self.folderEventHandler(transfer: transfer, flag: flag)
            let result = Result {
                direction == .download
                    ? try client.downloadDir(
                        remote: remote, local: local, excludes: excludes, onEvent: handler)
                    : try client.uploadDir(
                        local: local, remote: remote, excludes: excludes, onEvent: handler)
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
                    transfer.note(done, total: total)
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

    // MARK: - Commander helpers (keyboard + batch operations)

    func entries(in pane: PaneKind) -> [FileEntry] {
        pane == .local ? localEntries : remoteEntries
    }

    func selectedEntries(in pane: PaneKind) -> [FileEntry] {
        let ids = pane == .local ? localSelection : remoteSelection
        return entries(in: pane).filter { ids.contains($0.id) }
    }

    func navigateLocal(_ path: String) {
        let expanded = (path as NSString).expandingTildeInPath
        guard FileManager.default.fileExists(atPath: expanded) else {
            status = "No such directory: \(path)"
            loadLocal()
            return
        }
        localPath = expanded
        loadLocal()
    }

    func navigateRemote(_ path: String) {
        guard active.connected else { return }
        listRemote(path.isEmpty ? "/" : path)
    }

    /// F5: copy the focused pane's selection to the other side.
    func transferSelected() {
        requestTransfers(selectedEntries(in: focusedPane), from: focusedPane)
    }

    /// Start transfers with WinSCP-style overwrite protection: entries whose
    /// destination already holds a same-or-larger file prompt before
    /// clobbering (smaller partials auto-resume; folders merge as before).
    func requestTransfers(_ entries: [FileEntry], from pane: PaneKind) {
        var ready: [FileEntry] = []
        var conflicts: [FileEntry] = []
        for e in entries {
            let conflict: Bool
            if e.isDir {
                conflict = false
            } else if pane == .local {
                conflict = active.remoteEntries.contains {
                    !$0.isDir && $0.name == e.name && $0.size >= e.size
                }
            } else {
                let local = (localPath as NSString).appendingPathComponent(e.name)
                let len =
                    (try? FileManager.default.attributesOfItem(atPath: local)[.size]
                        as? NSNumber)?.uint64Value
                conflict = (len ?? 0) >= e.size && len != nil
            }
            if conflict { conflicts.append(e) } else { ready.append(e) }
        }
        for e in ready {
            if pane == .local { upload(e) } else { download(e) }
        }
        if !conflicts.isEmpty {
            overwritePrompt = (pane, conflicts)
        }
    }

    /// "Overwrite" chosen in the prompt: replace the existing destinations.
    func resolveOverwrite(overwrite: Bool) {
        guard let prompt = overwritePrompt else { return }
        overwritePrompt = nil
        guard overwrite else { return }
        for e in prompt.entries {
            if prompt.pane == .local { upload(e) } else { download(e) }
        }
    }

    /// Recursive remote search (Find Files).
    func runFind(mask: String) {
        let session = active
        guard session.connected, !mask.isEmpty else { return }
        let base = session.remotePath
        runBusy(on: session, "Searching \(base) for \(mask)…") { [client = session.client] in
            try client.find(base: base, mask: mask)
        } onSuccess: { [weak self] hits in
            self?.findResults = (mask, hits)
            self?.status = "Find: \(hits.count) match(es) for \(mask)"
        }
    }

    /// Open a new tab and connect from an sftp://, ftp://, ftps://, or s3:// URL.
    func openURL(_ url: URL) {
        guard let scheme = url.scheme?.lowercased() else { return }
        let p: Proto
        switch scheme {
        case "sftp": p = .sftp
        case "ftp": p = .ftp
        case "ftps": p = .ftps
        case "s3": p = .s3
        default: return
        }
        newTab()
        proto = p
        host = url.host ?? ""
        user = url.user ?? ""
        port = url.port.map(String.init) ?? String(Credentials_defaultPort(p))
        if p == .s3 {
            let parts = url.pathComponents.filter { $0 != "/" }
            bucket = parts.first ?? ""
        } else {
            let path = url.path.isEmpty ? "/" : url.path
            active.remotePath = path
            remotePath = path
        }
        let account = Keychain.account(proto: p, user: user, host: host, port: port)
        if let stored = Keychain.load(account: account) {
            password = stored
            showLogin = false
            connect()
        } else {
            status = "Loaded from URL — enter password and Connect"
        }
    }

    /// Show the Execute Command dialog (SFTP sessions only).
    func beginExecCommand() {
        guard active.connected, proto == .sftp else {
            status = "Execute Command is only available on connected SFTP sessions"
            return
        }
        execCmd = ""
        showExecDialog = true
    }

    /// Run `execCmd` on the active session and surface the result.
    func runExecCommand() {
        let session = active
        guard session.connected, proto == .sftp, !execCmd.isEmpty else { return }
        let cmd = execCmd
        showExecDialog = false
        runBusy(on: session, "Executing: \(cmd)…") { [client = session.client] in
            try client.execCommand(cmd)
        } onSuccess: { [weak self] result in
            self?.execResult = result
            self?.showExecResult = true
            self?.status = "Command exited \(result.exitCode)"
        }
    }

    /// Server-side duplicate of a remote file (same directory, new name).
    func copyRemoteFile(_ entry: FileEntry, toName: String) {
        let session = active
        guard session.connected, !entry.isDir, !toName.isEmpty else { return }
        let src = pathJoinPosix(session.remotePath, entry.name)
        let dst = pathJoinPosix(session.remotePath, toName)
        runBusy(on: session, "Copying \(entry.name)…") { [client = session.client] in
            try client.copyFile(src: src, dst: dst)
        } onSuccess: { [weak self] _ in
            self?.status = "Copied \(entry.name) → \(toName)"
            self?.refreshSession(session)
        }
    }

    /// Open an interactive SSH session to the current server in Terminal.
    func openTerminal() {
        guard active.connected, proto == .sftp, !host.isEmpty else {
            status = "Terminal sessions need a connected SFTP tab"
            return
        }
        let target = user.isEmpty ? host : "\(user)@\(host)"
        let ssh = "ssh -p \(port) \(target)"
        let script = "tell application \"Terminal\"\nactivate\ndo script \"\(ssh)\"\nend tell"
        let task = Process()
        task.launchPath = "/usr/bin/osascript"
        task.arguments = ["-e", script]
        do {
            try task.run()
            status = "Opened Terminal: \(ssh)"
        } catch {
            status = "Could not open Terminal: \(error.localizedDescription)"
        }
    }

    /// Copy an sftp:// URL for a remote entry to the clipboard.
    func copyRemoteURL(_ entry: FileEntry) {
        let scheme = proto == .ftp || proto == .ftps ? "ftp" : proto == .s3 ? "s3" : "sftp"
        let userPart = user.isEmpty ? "" : "\(user)@"
        let path = pathJoinPosix(active.remotePath, entry.name)
        let url = "\(scheme)://\(userPart)\(host):\(port)\(path)"
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(url, forType: .string)
        status = "Copied \(url)"
    }

    /// Upload arbitrary local files/folders (Finder drops) to the remote cwd.
    func uploadExternal(_ urls: [URL]) {
        let session = active
        guard session.connected else {
            status = "Connect first to upload"
            return
        }
        for url in urls {
            let name = url.lastPathComponent
            let remote = pathJoinPosix(session.remotePath, name)
            var isDir: ObjCBool = false
            FileManager.default.fileExists(atPath: url.path, isDirectory: &isDir)
            if isDir.boolValue {
                runFolderOp(
                    on: session, name: name, direction: .upload,
                    remote: remote, local: url.path
                ) { [weak self] in self?.refreshSession(session) }
            } else {
                let size =
                    (try? FileManager.default.attributesOfItem(atPath: url.path)[.size]
                        as? NSNumber)?.uint64Value ?? 0
                transferFile(
                    on: session, remote: remote, local: url.path, name: name, size: size,
                    direction: .upload
                ) { [weak self] in self?.refreshSession(session) }
            }
        }
    }

    /// F6: move = transfer, then delete the source on success.
    func moveSelected() {
        let pane = focusedPane
        let session = active
        guard session.connected else { return }
        for entry in selectedEntries(in: pane) {
            if pane == .local {
                let localFull = (localPath as NSString).appendingPathComponent(entry.name)
                let finish = { [weak self] in
                    try? FileManager.default.removeItem(atPath: localFull)
                    self?.loadLocal()
                    self?.refreshSession(session)
                }
                if entry.isDir {
                    runFolderOp(
                        on: session, name: entry.name, direction: .upload,
                        remote: session.remotePath + (session.remotePath.hasSuffix("/") ? "" : "/")
                            + entry.name,
                        local: localFull, onDone: finish)
                } else {
                    transferFile(
                        on: session,
                        remote: session.remotePath
                            + (session.remotePath.hasSuffix("/") ? "" : "/") + entry.name,
                        local: localFull, name: entry.name, size: entry.size,
                        direction: .upload, onDone: finish)
                }
            } else {
                let downloadEntry = entry
                let finish = { [weak self] in
                    self?.loadLocal()
                    self?.deleteRemote(downloadEntry)
                }
                if entry.isDir {
                    runFolderOp(
                        on: session, name: entry.name, direction: .download,
                        remote: session.remotePath
                            + (session.remotePath.hasSuffix("/") ? "" : "/") + entry.name,
                        local: (localPath as NSString).appendingPathComponent(entry.name),
                        onDone: finish)
                } else {
                    transferFile(
                        on: session,
                        remote: session.remotePath
                            + (session.remotePath.hasSuffix("/") ? "" : "/") + entry.name,
                        local: (localPath as NSString).appendingPathComponent(entry.name),
                        name: entry.name, size: entry.size,
                        direction: .download, onDone: finish)
                }
            }
        }
    }

    func deleteEntries(_ entries: [FileEntry], in pane: PaneKind) {
        for entry in entries {
            if pane == .local { deleteLocal(entry) } else { deleteRemote(entry) }
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

/// Weak hook so the terminate observer can reach the live AppState without
/// retaining it.
@MainActor
enum AppStateRegistry {
    static weak var shared: AppState?
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
