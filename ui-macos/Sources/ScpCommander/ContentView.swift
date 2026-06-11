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

// MARK: - Main window (WinSCP-style)

struct ContentView: View {
    @EnvironmentObject var state: AppState

    // Prompt state
    @State private var renameTarget: (pane: PaneKind, entry: FileEntry)?
    @State private var renameText = ""
    @State private var newFolderPane: PaneKind?
    @State private var newFolderText = ""
    @State private var deleteTarget: (pane: PaneKind, entries: [FileEntry])?
    @State private var propertiesTarget: (pane: PaneKind, entry: FileEntry)?
    @State private var keyMonitor: Any?

    var body: some View {
        VStack(spacing: 0) {
            TopBar()
            Divider()
            TabStrip()
            Divider()
            HSplitView {
                FilePane(
                    kind: "local",
                    title: "Local",
                    path: state.localPath,
                    entries: state.localEntries,
                    showRights: false,
                    showHidden: state.showHidden,
                    isFocused: state.focusedPane == .local,
                    selection: $state.localSelection,
                    onFocus: { state.focusedPane = .local },
                    onUp: { state.localUp() },
                    onRefresh: { state.loadLocal() },
                    onNavigate: { state.navigateLocal($0) },
                    onOpen: { state.openLocal($0) },
                    transferLabel: "Upload",
                    onTransfer: { state.upload($0) },
                    onDrop: { if $0.pane == "remote" { state.downloadByName($0.name) } },
                    onRename: { beginRename(.local, $0) },
                    onDelete: { deleteTarget = (.local, $0) },
                    onNewFolder: { beginNewFolder(.local) },
                    onEdit: nil,
                    onProperties: { propertiesTarget = (.local, $0) }
                )
                FilePane(
                    kind: "remote",
                    title: "Remote",
                    path: state.remotePath,
                    entries: state.remoteEntries,
                    showRights: true,
                    showHidden: state.showHidden,
                    isFocused: state.focusedPane == .remote,
                    selection: $state.remoteSelection,
                    onFocus: { state.focusedPane = .remote },
                    onUp: { state.remoteUp() },
                    onRefresh: { state.refreshRemote() },
                    onNavigate: { state.navigateRemote($0) },
                    onOpen: { state.openRemote($0) },
                    transferLabel: "Download",
                    onTransfer: { state.download($0) },
                    onDrop: { if $0.pane == "local" { state.uploadByName($0.name) } },
                    onRename: { beginRename(.remote, $0) },
                    onDelete: { deleteTarget = (.remote, $0) },
                    onNewFolder: { beginNewFolder(.remote) },
                    onEdit: { state.editRemote($0) },
                    onProperties: { propertiesTarget = (.remote, $0) }
                )
            }
            TransfersPanel(queue: state.transfers)
            Divider()
            StatusBar()
        }
        .frame(minWidth: 1000, minHeight: 560)
        .navigationTitle(
            state.isConnected
                ? "\(state.user.isEmpty ? "" : "\(state.user)@")\(state.host) — SCP Commander"
                : "SCP Commander")
        .onAppear { installKeyMonitor() }
        .sheet(isPresented: $state.showLogin) {
            LoginSheet().environmentObject(state)
        }
        .sheet(
            isPresented: Binding(
                get: { propertiesTarget != nil },
                set: { if !$0 { propertiesTarget = nil } }
            )
        ) {
            if let target = propertiesTarget {
                PropertiesSheet(pane: target.pane, entry: target.entry) {
                    propertiesTarget = nil
                }
                .environmentObject(state)
            }
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
            deleteTarget.map { t in
                t.entries.count == 1
                    ? "Delete \(t.entries[0].name)?" : "Delete \(t.entries.count) items?"
            } ?? "Delete?",
            isPresented: Binding(
                get: { deleteTarget != nil },
                set: { if !$0 { deleteTarget = nil } }
            )
        ) {
            Button("Delete", role: .destructive) {
                if let target = deleteTarget {
                    state.deleteEntries(target.entries, in: target.pane)
                }
                deleteTarget = nil
            }
            Button("Cancel", role: .cancel) { deleteTarget = nil }
        } message: {
            if deleteTarget?.entries.contains(where: \.isDir) == true {
                Text("Folders and everything inside them will be deleted.")
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

    // MARK: Keyboard commander (F5 copy, F6 move, F2 rename, Del, Tab, ⌫)

    private func installKeyMonitor() {
        guard keyMonitor == nil else { return }
        keyMonitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { event in
            handleKey(event) ? nil : event
        }
    }

    /// Returns true when the key was consumed.
    private func handleKey(_ event: NSEvent) -> Bool {
        // Don't steal keys from dialogs or while a text field is being edited.
        if state.showLogin || state.saveSitePrompt || renameTarget != nil
            || newFolderPane != nil || deleteTarget != nil || propertiesTarget != nil
        {
            return false
        }
        if NSApp.keyWindow?.firstResponder is NSTextView { return false }

        let pane = state.focusedPane
        switch event.keyCode {
        case 48:  // Tab — switch panes
            state.focusedPane = pane == .local ? .remote : .local
            return true
        case 51:  // Backspace — parent directory
            if pane == .local { state.localUp() } else { state.remoteUp() }
            return true
        case 96:  // F5 — copy selection to the other side
            state.transferSelected()
            return true
        case 97:  // F6 — move selection
            state.moveSelected()
            return true
        case 120:  // F2 — rename
            if let entry = state.selectedEntries(in: pane).first {
                beginRename(pane, entry)
            }
            return true
        case 117:  // Forward delete
            let selected = state.selectedEntries(in: pane)
            if !selected.isEmpty { deleteTarget = (pane, selected) }
            return true
        case 36:  // Return — open
            if let entry = state.selectedEntries(in: pane).first {
                if pane == .local { state.openLocal(entry) } else { state.openRemote(entry) }
            }
            return true
        default:
            return false
        }
    }
}

/// WinSCP-style session tab strip: one tab per server session.
private struct TabStrip: View {
    @EnvironmentObject var state: AppState

    var body: some View {
        HStack(spacing: 4) {
            ForEach(Array(state.tabTitles.enumerated()), id: \.offset) { index, title in
                let isActive = index == state.activeTab
                HStack(spacing: 4) {
                    Image(systemName: "network")
                        .font(.caption)
                        .foregroundStyle(isActive ? Color.accentColor : .secondary)
                    Text(title)
                        .font(.callout)
                        .lineLimit(1)
                    Button {
                        state.closeTab(index)
                    } label: {
                        Image(systemName: "xmark")
                            .font(.system(size: 8, weight: .bold))
                            .foregroundStyle(.secondary)
                    }
                    .buttonStyle(.borderless)
                    .help("Close tab")
                }
                .padding(.horizontal, 10)
                .padding(.vertical, 4)
                .background(
                    RoundedRectangle(cornerRadius: 5)
                        .fill(isActive ? Color.accentColor.opacity(0.15) : Color.clear)
                )
                .contentShape(Rectangle())
                .onTapGesture { state.selectTab(index) }
            }
            Button {
                state.newTab()
            } label: {
                Image(systemName: "plus")
            }
            .buttonStyle(.borderless)
            .help("New tab")
            Spacer()
        }
        .padding(.horizontal, 6)
        .padding(.vertical, 3)
    }
}

/// Slim main-window toolbar: session controls live in the Login dialog now.
private struct TopBar: View {
    @EnvironmentObject var state: AppState

    var body: some View {
        HStack(spacing: 10) {
            Button {
                state.showLogin = true
            } label: {
                Label("New Session", systemImage: "network")
            }

            Toggle(isOn: $state.showHidden) {
                Image(systemName: state.showHidden ? "eye" : "eye.slash")
            }
            .toggleStyle(.button)
            .help("Show hidden files")

            if state.isConnected {
                Text("\(state.user.isEmpty ? "" : "\(state.user)@")\(state.host)")
                    .font(.callout)
                    .foregroundStyle(.secondary)

                Menu {
                    Button("Local → Remote (upload changes)") { state.sync(download: false) }
                    Button("Remote → Local (download changes)") { state.sync(download: true) }
                } label: {
                    Label("Synchronize", systemImage: "arrow.triangle.2.circlepath")
                }
                .menuStyle(.borderlessButton)
                .fixedSize()
            }

            if state.busy { ProgressView().scaleEffect(0.6).frame(width: 18, height: 18) }
            Spacer()
            Text("F5 copy · F6 move · F2 rename · Tab panes")
                .font(.caption2)
                .foregroundStyle(.tertiary)
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 6)
    }
}

// MARK: - Login dialog (WinSCP-style)

private struct LoginSheet: View {
    @EnvironmentObject var state: AppState

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 0) {
                SitesSidebar(store: state.sites)
                    .frame(width: 210)
                Divider()
                SessionForm()
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            }
            Divider()
            HStack {
                Menu("Tools") {
                    Button("Import sites…") { state.importSites() }
                    Button("Import from WinSCP INI…") { state.importWinScp() }
                    Button("Export sites…") { state.exportSites() }
                }
                .menuStyle(.borderlessButton)
                .fixedSize()
                Button("Save site…") { state.beginSaveSite() }
                    .disabled(state.host.isEmpty && state.bucket.isEmpty)
                Spacer()
                if state.busy { ProgressView().scaleEffect(0.6).frame(width: 18, height: 18) }
                Button("Close") { state.showLogin = false }
                    .keyboardShortcut(.cancelAction)
                Button("Login") { state.connect() }
                    .keyboardShortcut(.defaultAction)
                    .disabled(
                        state.busy
                            || (state.proto == .s3 ? state.bucket.isEmpty : state.host.isEmpty))
            }
            .padding(10)
        }
        .frame(width: 760, height: 440)
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
    }
}

/// The right-hand "Session" form of the Login dialog.
private struct SessionForm: View {
    @EnvironmentObject var state: AppState

    var body: some View {
        let isS3 = state.proto == .s3
        let isSftp = state.proto == .sftp

        VStack(alignment: .leading, spacing: 10) {
            Text("Session").font(.headline).foregroundStyle(.secondary)

            Grid(alignment: .leading, horizontalSpacing: 10, verticalSpacing: 8) {
                GridRow {
                    Text("File protocol:")
                    Picker("", selection: $state.proto) {
                        ForEach(Proto.allCases, id: \.self) { Text($0.label).tag($0) }
                    }
                    .labelsHidden()
                    .frame(width: 110)
                }
                if isSftp {
                    GridRow {
                        Text("Authentication:")
                        Picker("", selection: $state.authMode) {
                            ForEach(AuthMode.allCases, id: \.self) { Text($0.label).tag($0) }
                        }
                        .labelsHidden()
                        .frame(width: 140)
                    }
                }
                GridRow {
                    Text(isS3 ? "Endpoint:" : "Host name:")
                    HStack {
                        TextField(isS3 ? "blank = AWS" : "host", text: $state.host)
                            .frame(minWidth: 220)
                        Text("Port:").foregroundStyle(.secondary)
                        TextField("port", text: $state.port).frame(width: 56)
                    }
                }
                GridRow {
                    Text(isS3 ? "Access key:" : "User name:")
                    TextField("", text: $state.user).frame(width: 180)
                }
                if isSftp && state.authMode == .keyFile {
                    GridRow {
                        Text("Private key:")
                        HStack {
                            TextField("key file", text: $state.keyPath).frame(minWidth: 220)
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
                        }
                    }
                    GridRow {
                        Text("Passphrase:")
                        SecureField("", text: $state.password).frame(width: 180)
                    }
                } else if !(isSftp && state.authMode == .agent) {
                    GridRow {
                        Text(isS3 ? "Secret key:" : "Password:")
                        SecureField("", text: $state.password).frame(width: 180)
                    }
                }
                if isS3 {
                    GridRow {
                        Text("Bucket:")
                        TextField("", text: $state.bucket).frame(width: 180)
                    }
                    GridRow {
                        Text("Region:")
                        TextField("us-east-1", text: $state.region).frame(width: 180)
                    }
                }
            }
            if state.proto == .ftp {
                Label(
                    "Plain FTP sends your password and data unencrypted — prefer SFTP or FTPS.",
                    systemImage: "exclamationmark.triangle"
                )
                .font(.caption)
                .foregroundStyle(.orange)
            }
            Spacer()
        }
        .padding(14)
    }
}

// MARK: - Sites sidebar (inside the Login dialog)

private struct SitesSidebar: View {
    @EnvironmentObject var state: AppState
    @ObservedObject var store: SitesStore

    @State private var renameTarget: Site?
    @State private var renameText = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
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
            Image(systemName: "display").foregroundStyle(.tint)
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

// MARK: - File pane (multi-column, multi-select, WinSCP-style)

private enum SortKey {
    case name, size, type, mtime
}

private struct FilePane: View {
    let kind: String
    let title: String
    let path: String
    let entries: [FileEntry]
    let showRights: Bool
    let showHidden: Bool
    let isFocused: Bool
    @Binding var selection: Set<FileEntry.ID>
    let onFocus: () -> Void
    let onUp: () -> Void
    let onRefresh: () -> Void
    let onNavigate: (String) -> Void
    let onOpen: (FileEntry) -> Void
    let transferLabel: String
    let onTransfer: (FileEntry) -> Void
    let onDrop: (DraggedFile) -> Void
    let onRename: (FileEntry) -> Void
    let onDelete: ([FileEntry]) -> Void
    let onNewFolder: () -> Void
    let onEdit: ((FileEntry) -> Void)?
    let onProperties: (FileEntry) -> Void

    @State private var sortKey: SortKey = .name
    @State private var ascending = true
    @State private var pathText = ""

    private var visible: [FileEntry] {
        showHidden ? entries : entries.filter { !$0.name.hasPrefix(".") }
    }

    private var selectedEntries: [FileEntry] {
        visible.filter { selection.contains($0.id) }
    }

    /// The clicked row plus the rest of the selection when it's part of it.
    private func batchTargets(_ entry: FileEntry) -> [FileEntry] {
        selection.contains(entry.id) ? selectedEntries : [entry]
    }

    /// Directories first (always), then the active column sort.
    private var sorted: [FileEntry] {
        visible.sorted { a, b in
            if a.isDir != b.isDir { return a.isDir }
            let result: Bool
            switch sortKey {
            case .name:
                result = a.name.localizedCaseInsensitiveCompare(b.name) == .orderedAscending
            case .size:
                result = a.size < b.size
            case .type:
                result =
                    a.typeDescription.localizedCaseInsensitiveCompare(b.typeDescription)
                    == .orderedAscending
            case .mtime:
                result = (a.mtime ?? .distantPast) < (b.mtime ?? .distantPast)
            }
            return ascending ? result : !result
        }
    }

    var body: some View {
        VStack(spacing: 0) {
            paneToolbar
            TextField("path", text: $pathText)
                .textFieldStyle(.plain)
                .font(.caption)
                .foregroundStyle(.secondary)
                .padding(.horizontal, 8)
                .padding(.bottom, 2)
                .onAppear { pathText = path }
                .onChange(of: path) { pathText = $0 }
                .onSubmit { onNavigate(pathText) }
            Divider()
            columnHeader
            Divider()
            List(selection: $selection) {
                ForEach(sorted) { entry in
                    row(for: entry)
                }
            }
            .listStyle(.inset(alternatesRowBackgrounds: true))
            .dropDestination(for: DraggedFile.self) { items, _ in
                for file in items where file.pane != kind { onDrop(file) }
                return items.contains { $0.pane != kind }
            }
        }
        .frame(minWidth: 380)
        .contentShape(Rectangle())
        .onTapGesture { onFocus() }
    }

    /// WinSCP-style per-pane command toolbar.
    private var paneToolbar: some View {
        HStack(spacing: 2) {
            Text(title)
                .font(.headline)
                .foregroundStyle(isFocused ? Color.accentColor : Color.primary)
                .padding(.trailing, 4)
            toolButton("arrow.up", "Parent directory", action: onUp)
            toolButton("arrow.clockwise", "Refresh", action: onRefresh)
            Divider().frame(height: 14)
            toolButton(
                kind == "local" ? "arrow.up.circle" : "arrow.down.circle",
                "\(transferLabel) (F5)", disabled: selectedEntries.isEmpty
            ) {
                for entry in selectedEntries { onTransfer(entry) }
            }
            if onEdit != nil {
                toolButton("pencil", "Edit (auto-upload on save)",
                    disabled: selectedEntries.first?.isDir != false
                ) {
                    if let e = selectedEntries.first { onEdit?(e) }
                }
            }
            toolButton("folder.badge.plus", "New folder", action: onNewFolder)
            toolButton("trash", "Delete", disabled: selectedEntries.isEmpty) {
                onDelete(selectedEntries)
            }
            Spacer()
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 4)
    }

    private func toolButton(
        _ symbol: String, _ help: String, disabled: Bool = false,
        action: @escaping () -> Void
    ) -> some View {
        Button(action: action) { Image(systemName: symbol) }
            .buttonStyle(.borderless)
            .disabled(disabled)
            .help(help)
    }

    /// Clickable, sortable column headers.
    private var columnHeader: some View {
        HStack(spacing: 0) {
            headerCell("Name", key: .name, alignment: .leading)
                .frame(maxWidth: .infinity, alignment: .leading)
            headerCell("Size", key: .size, alignment: .trailing)
                .frame(width: 76, alignment: .trailing)
            headerCell("Type", key: .type, alignment: .leading)
                .frame(width: 110, alignment: .leading)
            headerCell("Changed", key: .mtime, alignment: .leading)
                .frame(width: 118, alignment: .leading)
            if showRights {
                Text("Rights")
                    .font(.caption.bold())
                    .foregroundStyle(.secondary)
                    .frame(width: 80, alignment: .leading)
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 3)
    }

    private func headerCell(_ label: String, key: SortKey, alignment: Alignment) -> some View {
        Button {
            if sortKey == key {
                ascending.toggle()
            } else {
                sortKey = key
                ascending = true
            }
        } label: {
            HStack(spacing: 2) {
                Text(label).font(.caption.bold())
                if sortKey == key {
                    Image(systemName: ascending ? "chevron.up" : "chevron.down")
                        .font(.system(size: 8))
                }
            }
            .foregroundStyle(.secondary)
        }
        .buttonStyle(.plain)
    }

    private func row(for entry: FileEntry) -> some View {
        HStack(spacing: 0) {
            HStack(spacing: 6) {
                Image(systemName: entry.isSymlink
                    ? "arrow.right.circle"
                    : entry.isDir ? "folder.fill" : "doc")
                    .foregroundStyle(entry.isDir ? Color.accentColor : Color.secondary)
                    .frame(width: 16)
                Text(entry.name).lineLimit(1)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            Text(entry.isDir ? "" : humanSize(entry.size))
                .font(.caption.monospacedDigit())
                .foregroundStyle(.secondary)
                .frame(width: 76, alignment: .trailing)
            Text(entry.typeDescription)
                .font(.caption)
                .foregroundStyle(.secondary)
                .lineLimit(1)
                .frame(width: 110, alignment: .leading)
            Text(entry.mtime.map { changedFormatter.string(from: $0) } ?? "")
                .font(.caption.monospacedDigit())
                .foregroundStyle(.secondary)
                .frame(width: 118, alignment: .leading)
            if showRights {
                Text(entry.perms ?? "")
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
                    .frame(width: 80, alignment: .leading)
            }
        }
        .tag(entry.id)
        .contentShape(Rectangle())
        .simultaneousGesture(TapGesture(count: 1).onEnded { onFocus() })
        .onTapGesture(count: 2) { onOpen(entry) }
        .draggable(DraggedFile(pane: kind, name: entry.name))
        .contextMenu {
            if entry.isDir {
                Button("Open") { onOpen(entry) }
                Button("\(transferLabel) folder") { onTransfer(entry) }
            } else {
                let targets = batchTargets(entry)
                Button(targets.count > 1 ? "\(transferLabel) \(targets.count) items" : transferLabel)
                {
                    for t in targets { onTransfer(t) }
                }
                if let onEdit {
                    Button("Edit (auto-upload on save)") { onEdit(entry) }
                }
            }
            Divider()
            Button("Rename…") { onRename(entry) }
            Button("Properties…") { onProperties(entry) }
            Button("Delete", role: .destructive) { onDelete(batchTargets(entry)) }
        }
    }
}

let changedFormatter: DateFormatter = {
    let df = DateFormatter()
    df.dateFormat = "dd.MM.yyyy HH:mm"
    return df
}()

// MARK: - Properties dialog (WinSCP-style)

struct PropertiesSheet: View {
    @EnvironmentObject var state: AppState
    let pane: PaneKind
    let entry: FileEntry
    let onClose: () -> Void

    @State private var bits: [Bool] = Array(repeating: false, count: 9)
    @State private var loadedMode = false

    private var mode: UInt32 {
        bits.enumerated().reduce(0) { acc, item in
            item.element ? acc | (1 << (8 - item.offset)) : acc
        }
    }

    private var canChangeRights: Bool {
        pane == .local || state.proto != .s3
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 8) {
                Image(systemName: entry.isDir ? "folder.fill" : "doc")
                    .font(.title2)
                    .foregroundStyle(entry.isDir ? Color.accentColor : Color.secondary)
                Text(entry.name).font(.headline)
            }
            Grid(alignment: .leading, horizontalSpacing: 12, verticalSpacing: 6) {
                GridRow {
                    Text("Location:").foregroundStyle(.secondary)
                    Text(pane == .local ? state.localPath : state.remotePath)
                        .lineLimit(1).truncationMode(.head)
                }
                GridRow {
                    Text("Type:").foregroundStyle(.secondary)
                    Text(entry.typeDescription)
                }
                if !entry.isDir {
                    GridRow {
                        Text("Size:").foregroundStyle(.secondary)
                        Text("\(entry.size) bytes")
                    }
                }
                if let mtime = entry.mtime {
                    GridRow {
                        Text("Changed:").foregroundStyle(.secondary)
                        Text(changedFormatter.string(from: mtime))
                    }
                }
            }

            if canChangeRights {
                Divider()
                Text("Rights").font(.subheadline.bold())
                Grid(alignment: .leading, horizontalSpacing: 14, verticalSpacing: 4) {
                    GridRow {
                        Text("")
                        Text("Read").font(.caption).foregroundStyle(.secondary)
                        Text("Write").font(.caption).foregroundStyle(.secondary)
                        Text("Execute").font(.caption).foregroundStyle(.secondary)
                    }
                    ForEach(0..<3, id: \.self) { group in
                        GridRow {
                            Text(["Owner", "Group", "Others"][group])
                                .font(.caption)
                            ForEach(0..<3, id: \.self) { bit in
                                Toggle("", isOn: $bits[group * 3 + bit]).labelsHidden()
                            }
                        }
                    }
                }
                Text("Octal: \(String(format: "%03o", mode))")
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
            }

            HStack {
                Spacer()
                Button("Close") { onClose() }.keyboardShortcut(.cancelAction)
                if canChangeRights {
                    Button("Apply") {
                        switch pane {
                        case .local: state.chmodLocal(entry, mode: mode)
                        case .remote: state.chmodRemote(entry, mode: mode)
                        }
                        onClose()
                    }
                    .keyboardShortcut(.defaultAction)
                }
            }
        }
        .padding(16)
        .frame(width: 360)
        .onAppear {
            guard !loadedMode else { return }
            loadedMode = true
            let current: UInt32? =
                pane == .local ? state.localMode(of: entry) : entry.mode
            if let current {
                for i in 0..<9 { bits[i] = current & (1 << (8 - i)) != 0 }
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
            var parts: [String] = []
            if transfer.total > 0 {
                parts.append("\(humanSize(transfer.transferred)) / \(humanSize(transfer.total))")
            } else {
                parts.append(humanSize(transfer.transferred))
            }
            if transfer.speed > 1 {
                parts.append("\(humanSize(UInt64(transfer.speed)))/s")
            }
            if let eta = transfer.eta {
                parts.append(eta)
            }
            return parts.joined(separator: " · ")
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
