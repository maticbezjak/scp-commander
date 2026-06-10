import Foundation
import SwiftUI

/// Observable state for the whole window: connection fields, both pane
/// listings, and status. Blocking core calls run off the main thread.
@MainActor
final class AppState: ObservableObject {
    // Connection form
    @Published var proto: Proto = .sftp
    @Published var host = ""
    @Published var port = "22"
    @Published var user = ""
    @Published var password = ""

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

    var isConnected: Bool { client.isConnected }

    init() {
        loadLocal()
    }

    // MARK: - Saved sites

    func saveCurrentSite() {
        let name = host.isEmpty ? "New site" : "\(user.isEmpty ? "" : "\(user)@")\(host)"
        sites.add(Site(name: name, proto: proto, host: host, port: port, user: user))
        status = "Saved site “\(name)”"
    }

    func loadSite(_ site: Site) {
        proto = site.proto
        host = site.host
        port = site.port
        user = site.user
        password = ""  // entered fresh each connect
        status = "Loaded “\(site.name)” — enter password and Connect"
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

    // MARK: - Remote (core)

    func connect() {
        let p = proto
        let h = host
        let portNum = UInt16(port) ?? Credentials_defaultPort(p)
        let u = user
        let pw = password
        let path = remotePath
        runBusy("Connecting…") { [client] in
            try client.connect(proto: p, host: h, port: portNum, user: u, password: pw)
            return try client.listDir(path)
        } onSuccess: { [weak self] entries in
            self?.remoteEntries = entries
            self?.status = "Connected — \(path) (\(entries.count) items)"
        }
    }

    func openRemote(_ entry: FileEntry) {
        if entry.isDir {
            let newPath = pathJoinPosix(remotePath, entry.name)
            listRemote(newPath)
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
            self?.remotePath = path
            self?.remoteEntries = entries
            self?.status = "\(path) (\(entries.count) items)"
        }
    }

    func download(_ entry: FileEntry) {
        guard isConnected, !entry.isDir else { return }
        let remote = pathJoinPosix(remotePath, entry.name)
        let local = pathJoin(localPath, entry.name)
        let transfer = Transfer(name: entry.name, direction: .download)
        transfer.total = entry.size
        transfers.add(transfer)

        queue.async { [weak self, client] in
            do {
                _ = try client.download(remote: remote, local: local) { done, total in
                    DispatchQueue.main.async {
                        transfer.transferred = done
                        if total > 0 { transfer.total = total }
                    }
                }
                DispatchQueue.main.async {
                    transfer.state = .done
                    self?.status = "Downloaded \(entry.name)"
                    self?.loadLocal()
                }
            } catch {
                DispatchQueue.main.async {
                    transfer.state = .failed(error.localizedDescription)
                    self?.status = "Error: \(error.localizedDescription)"
                }
            }
        }
    }

    func upload(_ entry: FileEntry) {
        guard !entry.isDir else { return }
        guard isConnected else {
            status = "Connect first to upload"
            return
        }
        let local = pathJoin(localPath, entry.name)
        let remote = pathJoinPosix(remotePath, entry.name)
        let transfer = Transfer(name: entry.name, direction: .upload)
        transfer.total = entry.size
        transfers.add(transfer)

        queue.async { [weak self, client] in
            do {
                _ = try client.upload(local: local, remote: remote) { done, total in
                    DispatchQueue.main.async {
                        transfer.transferred = done
                        if total > 0 { transfer.total = total }
                    }
                }
                DispatchQueue.main.async {
                    transfer.state = .done
                    self?.status = "Uploaded \(entry.name)"
                    if let self { self.listRemote(self.remotePath) }
                }
            } catch {
                DispatchQueue.main.async {
                    transfer.state = .failed(error.localizedDescription)
                    self?.status = "Error: \(error.localizedDescription)"
                }
            }
        }
    }

    // Drag-and-drop entry points (look the entry up by name in its pane).
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
        onSuccess: @escaping (T) -> Void
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
                case .failure(let error): self.status = "Error: \(error.localizedDescription)"
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
