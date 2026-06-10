import AppKit
import SwiftUI
import UniformTypeIdentifiers

/// Payload dragged between panes. `pane` records the source so a drop on the
/// opposite pane knows whether it's an upload or a download.
struct DraggedFile: Codable, Transferable {
    let pane: String
    let name: String

    static var transferRepresentation: some TransferRepresentation {
        CodableRepresentation(contentType: .data)
    }
}

/// Which pane a prompt (rename / new folder / delete) targets.
enum PaneKind {
    case local
    case remote
}

struct ContentView: View {
    @EnvironmentObject var state: AppState

    // Prompt state
    @State private var renameTarget: (pane: PaneKind, entry: FileEntry)?
    @State private var renameText = ""
    @State private var newFolderPane: PaneKind?
    @State private var newFolderText = ""
    @State private var deleteTarget: (pane: PaneKind, entry: FileEntry)?

    var body: some View {
        HStack(spacing: 0) {
            SitesSidebar(store: state.sites)
                .frame(width: 190)
            Divider()
            VStack(spacing: 0) {
                ConnectionBar()
                Divider()
                HSplitView {
                    FilePane(
                        kind: "local",
                        title: "Local",
                        path: state.localPath,
                        entries: state.localEntries,
                        onUp: { state.localUp() },
                        onOpen: { state.openLocal($0) },
                        transferLabel: "Upload →",
                        onTransfer: { state.upload($0) },
                        onDrop: { if $0.pane == "remote" { state.downloadByName($0.name) } },
                        onRename: { beginRename(.local, $0) },
                        onDelete: { deleteTarget = (.local, $0) },
                        onNewFolder: { beginNewFolder(.local) },
                        onEdit: nil
                    )
                    FilePane(
                        kind: "remote",
                        title: "Remote",
                        path: state.remotePath,
                        entries: state.remoteEntries,
                        onUp: { state.remoteUp() },
                        onOpen: { state.openRemote($0) },
                        transferLabel: "← Download",
                        onTransfer: { state.download($0) },
                        onDrop: { if $0.pane == "local" { state.uploadByName($0.name) } },
                        onRename: { beginRename(.remote, $0) },
                        onDelete: { deleteTarget = (.remote, $0) },
                        onNewFolder: { beginNewFolder(.remote) },
                        onEdit: { state.editRemote($0) }
                    )
                }
                TransfersPanel(queue: state.transfers)
                Divider()
                StatusBar()
            }
        }
        .frame(minWidth: 1000, minHeight: 560)
        .sheet(isPresented: $state.saveSitePrompt) {
            SaveSiteSheet().environmentObject(state)
        }
        .alert(
            "Unknown server host key",
            isPresented: Binding(
                get: { state.hostKeyPrompt != nil },
                set: { if !$0 { state.hostKeyPrompt = nil } }
            )
        ) {
            Button("Trust & Connect") {
                if let fp = state.hostKeyPrompt {
                    state.hostKeyPrompt = nil
                    state.connect(trustingFingerprint: fp)
                }
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text(
                """
                This server has not been seen before. Its key fingerprint is:

                \(state.hostKeyPrompt ?? "")

                If you expected a first-time connection, verify this matches \
                the server's actual fingerprint before trusting it.
                """)
        }
        .alert(
            "Rename",
            isPresented: Binding(
                get: { renameTarget != nil },
                set: { if !$0 { renameTarget = nil } }
            )
        ) {
            TextField("New name", text: $renameText)
            Button("Rename") {
                if let target = renameTarget {
                    switch target.pane {
                    case .local: state.renameLocal(target.entry, to: renameText)
                    case .remote: state.renameRemote(target.entry, to: renameText)
                    }
                }
                renameTarget = nil
            }
            Button("Cancel", role: .cancel) { renameTarget = nil }
        }
        .alert(
            "New folder",
            isPresented: Binding(
                get: { newFolderPane != nil },
                set: { if !$0 { newFolderPane = nil } }
            )
        ) {
            TextField("Folder name", text: $newFolderText)
            Button("Create") {
                switch newFolderPane {
                case .local: state.newLocalFolder(named: newFolderText)
                case .remote: state.newRemoteFolder(named: newFolderText)
                case nil: break
                }
                newFolderPane = nil
            }
            Button("Cancel", role: .cancel) { newFolderPane = nil }
        }
        .alert(
            "Delete \(deleteTarget?.entry.name ?? "")?",
            isPresented: Binding(
                get: { deleteTarget != nil },
                set: { if !$0 { deleteTarget = nil } }
            )
        ) {
            Button("Delete", role: .destructive) {
                if let target = deleteTarget {
                    switch target.pane {
                    case .local: state.deleteLocal(target.entry)
                    case .remote: state.deleteRemote(target.entry)
                    }
                }
                deleteTarget = nil
            }
            Button("Cancel", role: .cancel) { deleteTarget = nil }
        } message: {
            if deleteTarget?.entry.isDir == true {
                Text("The folder and everything inside it will be deleted.")
            } else {
                Text("This cannot be undone.")
            }
        }
    }

    private func beginRename(_ pane: PaneKind, _ entry: FileEntry) {
        renameText = entry.name
        renameTarget = (pane, entry)
    }

    private func beginNewFolder(_ pane: PaneKind) {
        newFolderText = ""
        newFolderPane = pane
    }
}

// MARK: - Sites sidebar

private struct SitesSidebar: View {
    @EnvironmentObject var state: AppState
    @ObservedObject var store: SitesStore

    @State private var renameTarget: Site?
    @State private var renameText = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                Text("Sites").font(.headline)
                Spacer()
                Button(action: { state.beginSaveSite() }) {
                    Image(systemName: "plus")
                }
                .buttonStyle(.borderless)
                .help("Save current session as a site (use Folder/Name to group)")
            }
            .padding(8)
            Divider()

            List {
                ForEach(store.folders, id: \.self) { folder in
                    Section {
                        ForEach(store.sites(in: folder)) { site in
                            row(for: site)
                        }
                    } header: {
                        if let folder {
                            Label(folder, systemImage: "folder")
                        }
                    }
                }
            }
            .listStyle(.sidebar)
        }
        .alert(
            "Rename site",
            isPresented: Binding(
                get: { renameTarget != nil },
                set: { if !$0 { renameTarget = nil } }
            )
        ) {
            TextField("Name", text: $renameText)
            Button("Rename") {
                if let site = renameTarget { state.renameSite(site, to: renameText) }
                renameTarget = nil
            }
            Button("Cancel", role: .cancel) { renameTarget = nil }
        } message: {
            Text("Use Folder/Name to group sites into a folder.")
        }
    }

    /// WinSCP behavior: single click edits (fills the form), double click
    /// logs in, right click for the full menu.
    private func row(for site: Site) -> some View {
        HStack(spacing: 6) {
            Image(systemName: "bookmark.fill").foregroundStyle(.tint)
            VStack(alignment: .leading, spacing: 1) {
                Text(site.displayName).lineLimit(1)
                Text(site.proto.label).font(.caption2).foregroundStyle(.secondary)
            }
        }
        .contentShape(Rectangle())
        .onTapGesture(count: 2) { state.login(site) }
        .onTapGesture(count: 1) { state.loadSite(site) }
        .contextMenu {
            Button("Login") { state.login(site) }
            Button("Edit") { state.loadSite(site) }
            Divider()
            Button("Rename…") {
                renameText = site.name
                renameTarget = site
            }
            Button("Delete", role: .destructive) { state.removeSite(site) }
        }
    }
}

/// WinSCP's "Save session as site" dialog: name (Folder/Name groups) and an
/// explicit opt-in for password storage.
private struct SaveSiteSheet: View {
    @EnvironmentObject var state: AppState

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Save session as site").font(.headline)
            TextField("Site name (Folder/Name to group)", text: $state.saveSiteName)
                .frame(width: 280)
            if state.proto != .sftp || state.authMode == .password {
                Toggle("Save password in Keychain", isOn: $state.saveSitePassword)
                    .disabled(state.password.isEmpty)
            }
            HStack {
                Spacer()
                Button("Cancel") { state.saveSitePrompt = false }
                    .keyboardShortcut(.cancelAction)
                Button("Save") { state.confirmSaveSite() }
                    .keyboardShortcut(.defaultAction)
                    .disabled(state.saveSiteName.trimmingCharacters(in: .whitespaces).isEmpty)
            }
        }
        .padding(16)
    }
}

// MARK: - Connection bar

private struct ConnectionBar: View {
    @EnvironmentObject var state: AppState

    var body: some View {
        HStack(spacing: 8) {
            let isS3 = state.proto == .s3
            let isSftp = state.proto == .sftp

            Picker("", selection: $state.proto) {
                ForEach(Proto.allCases, id: \.self) { Text($0.label).tag($0) }
            }
            .labelsHidden()
            .frame(width: 80)

            if isSftp {
                Picker("", selection: $state.authMode) {
                    ForEach(AuthMode.allCases, id: \.self) { Text($0.label).tag($0) }
                }
                .labelsHidden()
                .frame(width: 100)
            }

            TextField(isS3 ? "access key" : "user", text: $state.user).frame(width: 100)
            Text("@").foregroundStyle(.secondary)
            TextField(isS3 ? "endpoint (blank = AWS)" : "host", text: $state.host)
                .frame(minWidth: 120)
            TextField("port", text: $state.port).frame(width: 50)

            if isSftp && state.authMode == .keyFile {
                TextField("key file", text: $state.keyPath).frame(width: 120)
                Button("…") {
                    let panel = NSOpenPanel()
                    panel.canChooseFiles = true
                    panel.canChooseDirectories = false
                    panel.directoryURL = FileManager.default
                        .homeDirectoryForCurrentUser.appendingPathComponent(".ssh")
                    if panel.runModal() == .OK {
                        state.keyPath = panel.url?.path ?? ""
                    }
                }
                .help("Choose a private key")
                SecureField("passphrase", text: $state.password).frame(width: 110)
            } else if !(isSftp && state.authMode == .agent) {
                SecureField(isS3 ? "secret key" : "password", text: $state.password)
                    .frame(width: 120)
            }

            if isS3 {
                TextField("bucket", text: $state.bucket).frame(width: 100)
                TextField("region", text: $state.region).frame(width: 80)
            }

            Button(action: { state.connect() }) {
                Text(state.isConnected ? "Reconnect" : "Connect")
            }
            .keyboardShortcut(.return, modifiers: [])
            .disabled(state.busy || (isS3 ? state.bucket.isEmpty : state.host.isEmpty))

            if state.isConnected {
                Menu {
                    Button("Local → Remote (upload changes)") { state.sync(download: false) }
                    Button("Remote → Local (download changes)") { state.sync(download: true) }
                } label: {
                    Image(systemName: "arrow.triangle.2.circlepath")
                }
                .menuStyle(.borderlessButton)
                .frame(width: 40)
                .help("Synchronize the current folders")
            }

            if state.busy { ProgressView().scaleEffect(0.6).frame(width: 18, height: 18) }
            Spacer()
        }
        .padding(8)
    }
}

// MARK: - File pane

private struct FilePane: View {
    let kind: String
    let title: String
    let path: String
    let entries: [FileEntry]
    let onUp: () -> Void
    let onOpen: (FileEntry) -> Void
    let transferLabel: String
    let onTransfer: (FileEntry) -> Void
    let onDrop: (DraggedFile) -> Void
    let onRename: (FileEntry) -> Void
    let onDelete: (FileEntry) -> Void
    let onNewFolder: () -> Void
    let onEdit: ((FileEntry) -> Void)?

    @State private var selection: FileEntry.ID?

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Text(title).font(.headline)
                Spacer()
                Button(action: onNewFolder) { Image(systemName: "folder.badge.plus") }
                    .help("New folder")
                    .buttonStyle(.borderless)
                Button(action: onUp) { Image(systemName: "arrow.up") }
                    .help("Parent directory")
                    .buttonStyle(.borderless)
            }
            .padding(.horizontal, 8).padding(.vertical, 4)

            Text(path)
                .font(.caption)
                .foregroundStyle(.secondary)
                .lineLimit(1)
                .truncationMode(.head)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, 8)

            Divider()

            List(selection: $selection) {
                ForEach(entries) { entry in
                    EntryRow(entry: entry)
                        .tag(entry.id)
                        .contentShape(Rectangle())
                        .onTapGesture(count: 2) { onOpen(entry) }
                        .draggable(DraggedFile(pane: kind, name: entry.name))
                        .contextMenu {
                            if entry.isDir {
                                Button("Open") { onOpen(entry) }
                                Button(transferLabel.replacingOccurrences(
                                    of: "→", with: "folder →"
                                ).replacingOccurrences(of: "←", with: "← folder")) {
                                    onTransfer(entry)
                                }
                            } else {
                                Button(transferLabel) { onTransfer(entry) }
                                if let onEdit {
                                    Button("Edit (auto-upload on save)") { onEdit(entry) }
                                }
                            }
                            Divider()
                            Button("Rename…") { onRename(entry) }
                            Button("Delete", role: .destructive) { onDelete(entry) }
                        }
                }
            }
            .listStyle(.inset(alternatesRowBackgrounds: true))
            .dropDestination(for: DraggedFile.self) { items, _ in
                for file in items where file.pane != kind { onDrop(file) }
                return items.contains { $0.pane != kind }
            }
        }
        .frame(minWidth: 360)
    }
}

private struct EntryRow: View {
    let entry: FileEntry

    var body: some View {
        HStack(spacing: 6) {
            Image(systemName: entry.isDir ? "folder.fill" : "doc")
                .foregroundStyle(entry.isDir ? Color.accentColor : Color.secondary)
                .frame(width: 16)
            Text(entry.name).lineLimit(1)
            Spacer()
            if !entry.isDir {
                Text(humanSize(entry.size))
                    .font(.caption.monospacedDigit())
                    .foregroundStyle(.secondary)
            }
        }
    }
}

// MARK: - Transfers panel

private struct TransfersPanel: View {
    @ObservedObject var queue: TransferQueue

    var body: some View {
        let items = queue.items
        if !items.isEmpty {
            VStack(spacing: 0) {
                Divider()
                HStack {
                    Text("Transfers").font(.caption).bold()
                    Spacer()
                    Button("Clear finished") { queue.clearFinished() }
                        .buttonStyle(.borderless)
                        .font(.caption)
                }
                .padding(.horizontal, 8).padding(.vertical, 2)

                ScrollView {
                    VStack(spacing: 2) {
                        ForEach(items) { TransferRow(transfer: $0) }
                    }
                    .padding(.horizontal, 8)
                }
                .frame(maxHeight: 120)
            }
        }
    }
}

private struct TransferRow: View {
    @ObservedObject var transfer: Transfer

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: transfer.direction.symbol)
            VStack(alignment: .leading, spacing: 0) {
                Text(transfer.name).lineLimit(1)
                if let current = transfer.currentFile, transfer.state == .active {
                    Text(current)
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.head)
                }
            }
            .frame(width: 180, alignment: .leading)
            progress
            Spacer()
            Text(detail).font(.caption.monospacedDigit()).foregroundStyle(.secondary)
            if transfer.state == .active {
                Button {
                    transfer.cancelFlag.cancel()
                } label: {
                    Image(systemName: "xmark.circle.fill").foregroundStyle(.secondary)
                }
                .buttonStyle(.borderless)
                .help("Cancel")
            }
        }
    }

    @ViewBuilder
    private var progress: some View {
        switch transfer.state {
        case .active:
            if let f = transfer.fraction {
                ProgressView(value: f).frame(width: 160)
            } else {
                ProgressView().scaleEffect(0.5).frame(width: 40)
            }
        case .done:
            Image(systemName: "checkmark.circle.fill").foregroundStyle(.green)
        case .cancelled:
            Image(systemName: "slash.circle").foregroundStyle(.orange)
        case .failed:
            Image(systemName: "xmark.octagon.fill").foregroundStyle(.red)
        }
    }

    private var detail: String {
        switch transfer.state {
        case .failed(let msg): return msg
        case .cancelled: return "cancelled"
        case .done:
            let files = transfer.filesDone > 0 ? "\(transfer.filesDone) files · " : ""
            return files + humanSize(max(transfer.total, transfer.transferred))
        case .active:
            if transfer.total > 0 {
                return "\(humanSize(transfer.transferred)) / \(humanSize(transfer.total))"
            }
            return humanSize(transfer.transferred)
        }
    }
}

private struct StatusBar: View {
    @EnvironmentObject var state: AppState
    var body: some View {
        HStack {
            Text(state.status).font(.caption).lineLimit(1)
            Spacer()
        }
        .padding(.horizontal, 8).padding(.vertical, 4)
    }
}

private func humanSize(_ bytes: UInt64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB"]
    var value = Double(bytes)
    var i = 0
    while value >= 1024 && i < units.count - 1 {
        value /= 1024
        i += 1
    }
    return i == 0 ? "\(bytes) B" : String(format: "%.1f %@", value, units[i])
}
