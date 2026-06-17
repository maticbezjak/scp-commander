import AppKit
import SwiftUI
import UniformTypeIdentifiers

/// Payload dragged between panes. `pane` records the source so a drop on the
/// opposite pane knows whether it's an upload or a download. `path` is the
/// absolute source path so a drag *out* to Finder can fetch the file.
struct DraggedFile: Codable, Transferable {
    let pane: String
    let name: String
    var path: String = ""
    var isDir: Bool = false

    static var transferRepresentation: some TransferRepresentation {
        // Same-app drags between the two panes use this typed payload.
        CodableRepresentation(
            contentType: UTType(exportedAs: "com.manto.scp-commander.dragged-file")
        )
        // Dragging onto Finder/Desktop exports a real file: local items hand
        // over their URL, remote items download to a temp copy on demand.
        FileRepresentation(exportedContentType: .data) { item in
            if item.pane == "local" {
                return SentTransferredFile(URL(fileURLWithPath: item.path))
            }
            guard let app = await AppStateRegistry.shared else {
                throw CoreError(message: "not connected")
            }
            let url = try await app.dragOutDownload(
                path: item.path, name: item.name, isDir: item.isDir)
            return SentTransferredFile(url)
        }
    }
}

/// Installs an NSEvent local monitor on the window that fires `action` on
/// double-click within the view's bounds. Avoids competing with List's
/// native selection gesture (which `onTapGesture(count:2)` blocks).
private struct DoubleClickMonitor: NSViewRepresentable {
    let action: () -> Void

    func makeCoordinator() -> Coordinator { Coordinator(action: action) }
    func makeNSView(context: Context) -> NSView {
        context.coordinator.hostView = context.coordinator.hostView ?? NSView()
        return context.coordinator.hostView!
    }
    func updateNSView(_ nsView: NSView, context: Context) {
        context.coordinator.action = action
    }

    final class Coordinator {
        var action: () -> Void
        var hostView: NSView?
        private var monitor: Any?

        init(action: @escaping () -> Void) {
            self.action = action
            monitor = NSEvent.addLocalMonitorForEvents(matching: .leftMouseDown) { [weak self] event in
                guard event.clickCount == 2,
                      let view = self?.hostView,
                      let window = view.window else { return event }
                let loc = view.convert(event.locationInWindow, from: nil)
                if view.bounds.contains(loc) {
                    self?.action()
                }
                return event
            }
        }
        deinit { if let m = monitor { NSEvent.removeMonitor(m) } }
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
    @State private var copyTarget: FileEntry?
    @State private var copyNameText = ""
    @State private var keyMonitor: Any?
    // Type-ahead: letters typed within 1s jump to the first matching entry.
    @State private var typeAhead = ""
    @State private var typeAheadAt = Date.distantPast

    @ViewBuilder private var localPane: some View {
        FilePane(
            kind: "local",
            title: "Local",
            path: state.localPath,
            entries: state.localEntries,
            showRights: false,
            showHidden: state.showHidden,
            isFocused: state.localFocused,
            selection: $state.localSelection,
            onFocus: { if state.focusedPane != .local { state.focusedPane = .local } },
            onUp: { state.localUp() },
            onRefresh: { state.loadLocal() },
            onNavigate: { state.navigateLocal($0) },
            onOpen: { state.openLocal($0) },
            transferLabel: "Upload",
            onTransfer: { state.requestTransfers([$0], from: .local) },
            onDrop: { if $0.pane == "remote" { state.downloadByName($0.name) } },
            onRename: { beginRename(.local, $0) },
            onDelete: { deleteTarget = (.local, $0) },
            onNewFolder: { beginNewFolder(.local) },
            onEdit: nil,
            onCopyURL: nil,
            onProperties: { propertiesTarget = (.local, $0) },
            onExternalDrop: nil,
            onView: { state.viewFile($0, pane: .local) },
            onClipboardCopy: { state.copySelection(from: .local) },
            onClipboardPaste: { state.paste(into: .local) },
            canPaste: state.paneClipboard?.sourcePane == .remote,
            onBack: { state.goBack(.local) },
            onForward: { state.goForward(.local) },
            onHome: { state.goHome(.local) },
            canBack: state.canGoBack(.local),
            canForward: state.canGoForward(.local),
            recents: state.localRecents,
            focusRequested: state.focusPathBar == .local,
            onFocusHandled: { state.focusPathBar = nil }
        )
    }

    @ViewBuilder private var remotePane: some View {
        FilePane(
            kind: "remote",
            title: "Remote",
            path: state.remotePath,
            entries: state.remoteEntries,
            showRights: true,
            showHidden: state.showHidden,
            isFocused: !state.localFocused,
            selection: $state.remoteSelection,
            onFocus: { if state.focusedPane != .remote { state.focusedPane = .remote } },
            onUp: { state.remoteUp() },
            onRefresh: { state.refreshRemote() },
            onNavigate: { state.navigateRemote($0) },
            onOpen: { state.openRemote($0) },
            transferLabel: "Download",
            onTransfer: { state.requestTransfers([$0], from: .remote) },
            onDrop: { if $0.pane == "local" { state.uploadByName($0.name) } },
            onRename: { beginRename(.remote, $0) },
            onDelete: { deleteTarget = (.remote, $0) },
            onNewFolder: { beginNewFolder(.remote) },
            onEdit: { state.editRemote($0) },
            onCopyURL: { state.copyRemoteURL($0) },
            onProperties: { propertiesTarget = (.remote, $0) },
            onExternalDrop: { state.uploadExternal($0) },
            onCopyFile: { e in copyNameText = e.name; copyTarget = e },
            onExec: state.proto == .sftp ? { _ in state.beginExecCommand() } : nil,
            onView: { state.viewFile($0, pane: .remote) },
            onClipboardCopy: { state.copySelection(from: .remote) },
            onClipboardPaste: { state.paste(into: .remote) },
            canPaste: state.paneClipboard?.sourcePane == .local,
            onBack: { state.goBack(.remote) },
            onForward: { state.goForward(.remote) },
            onHome: { state.goHome(.remote) },
            canBack: state.canGoBack(.remote),
            canForward: state.canGoForward(.remote),
            recents: state.remoteRecents,
            focusRequested: state.focusPathBar == .remote,
            onFocusHandled: { state.focusPathBar = nil }
        )
    }

    var body: some View {
        VStack(spacing: 0) {
            TopBar()
            Divider()
            TabStrip()
            Divider()
            HSplitView {
                localPane
                remotePane
            }
            Divider()
            CommandBar()
            Divider()
            StatusBar()
        }
        .frame(minWidth: 1000, minHeight: 560)
        .navigationTitle(
            state.isConnected
                ? "\(state.user.isEmpty ? "" : "\(state.user)@")\(state.host) — SCP Commander"
                : "SCP Commander")
        .onAppear { installKeyMonitor() }
        .onChange(of: state.pendingMenuAction) { action in
            guard let action else { return }
            state.pendingMenuAction = nil
            handleMenuAction(action)
        }
        .sheet(isPresented: $state.showLogin) {
            LoginSheet().environmentObject(state)
        }
        .sheet(
            isPresented: Binding(
                get: { state.syncPreview != nil },
                set: { if !$0 { state.syncPreview = nil } }
            )
        ) {
            SyncPreviewSheet().environmentObject(state)
        }
        .sheet(isPresented: $state.showFind) {
            FindSheet().environmentObject(state)
        }
        .confirmationDialog(
            state.overwritePrompt.map { p in
                p.entries.count == 1
                    ? "\(p.entries[0].name) already exists at the destination."
                    : "\(p.entries.count) files already exist at the destination."
            } ?? "",
            isPresented: Binding(
                get: { state.overwritePrompt != nil },
                set: { if !$0 { state.overwritePrompt = nil } }
            ),
            titleVisibility: .visible
        ) {
            Button("Overwrite", role: .destructive) { state.resolveOverwrite(.overwrite) }
            Button("Overwrite only newer") { state.resolveOverwrite(.onlyNewer) }
            Button("Skip existing") { state.resolveOverwrite(.skip) }
            Button("Cancel", role: .cancel) { state.overwritePrompt = nil }
        } message: {
            if let p = state.overwritePrompt {
                Text(overwriteDetail(p))
            }
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
        .sheet(isPresented: $state.showCustomCommands) {
            CustomCommandsSheet { state.showCustomCommands = false }
                .environmentObject(state)
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
        .sheet(isPresented: $state.showExecDialog) {
            ExecCommandSheet().environmentObject(state)
        }
        .sheet(isPresented: $state.showExecResult) {
            ExecResultSheet().environmentObject(state)
        }
        .sheet(
            isPresented: Binding(
                get: { state.reconnectMessage != nil },
                set: { if !$0 { state.dismissReconnect() } }
            )
        ) {
            ReconnectSheet().environmentObject(state)
        }
        .sheet(
            isPresented: Binding(
                get: { state.viewerContent != nil },
                set: { if !$0 { state.viewerContent = nil } }
            )
        ) {
            if let v = state.viewerContent {
                ViewerSheet(name: v.name, text: v.text) { state.viewerContent = nil }
            }
        }
        .alert(
            "Duplicate as…",
            isPresented: Binding(
                get: { copyTarget != nil },
                set: { if !$0 { copyTarget = nil } }
            )
        ) {
            TextField("New name", text: $copyNameText)
            Button("Duplicate") {
                if let e = copyTarget { state.copyRemoteFile(e, toName: copyNameText) }
                copyTarget = nil
            }
            Button("Cancel", role: .cancel) { copyTarget = nil }
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

    /// WinSCP-style overwrite detail: source vs target size + mtime, with a
    /// "newer"/"older" hint. Multi-file prompts summarize the newer count.
    private func overwriteDetail(_ p: (pane: PaneKind, entries: [FileEntry])) -> String {
        func stamp(_ d: Date?) -> String { d.map { changedFormatter.string(from: $0) } ?? "?" }
        // Folders merge: the chosen action applies per file inside them.
        if p.entries.contains(where: { $0.isDir }) {
            let folders = p.entries.filter { $0.isDir }.count
            let what = folders == p.entries.count
                ? (folders == 1 ? "This folder" : "These folders")
                : "Some items"
            return "\(what) already exist at the destination. "
                + "Your choice applies to each file inside:\n"
                + "Overwrite — replace all · Only newer — replace older files · "
                + "Skip — keep existing, copy only new files."
        }
        if p.entries.count == 1 {
            let e = p.entries[0]
            guard let dest = state.destinationInfo(for: e, pane: p.pane) else { return "" }
            let hint: String
            if let s = e.mtime, let t = dest.mtime {
                hint = s > t ? " — source is newer" : s < t ? " — source is older" : " — same time"
            } else {
                hint = ""
            }
            return """
                Source: \(humanSize(e.size)) · \(stamp(e.mtime))
                Target: \(humanSize(dest.size)) · \(stamp(dest.mtime))\(hint)
                """
        }
        let newer = p.entries.filter { e in
            guard let s = e.mtime, let t = state.destinationInfo(for: e, pane: p.pane)?.mtime
            else { return false }
            return s > t
        }.count
        return "\(newer) of \(p.entries.count) source files are newer than their targets."
    }

    private func beginRename(_ pane: PaneKind, _ entry: FileEntry) {
        renameText = entry.name
        renameTarget = (pane, entry)
    }

    private func beginNewFolder(_ pane: PaneKind) {
        newFolderText = ""
        newFolderPane = pane
    }

    /// Dispatch a Files-menu action onto the focused pane's selection.
    private func handleMenuAction(_ action: AppState.MenuAction) {
        let pane = state.focusedPane
        let selected = state.selectedEntries(in: pane)
        switch action {
        case .rename:
            if let entry = selected.first { beginRename(pane, entry) }
        case .newFolder:
            beginNewFolder(pane)
        case .delete:
            if !selected.isEmpty { deleteTarget = (pane, selected) }
        case .properties:
            if let entry = selected.first { propertiesTarget = (pane, entry) }
        case .duplicate:
            // Server-side duplicate is remote-only.
            if pane == .remote, let entry = selected.first, !entry.isDir {
                copyNameText = entry.name
                copyTarget = entry
            }
        }
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
            || copyTarget != nil || state.showExecDialog || state.showExecResult
        {
            return false
        }
        if NSApp.keyWindow?.firstResponder is NSTextView { return false }

        let pane = state.focusedPane
        // ⌘C / ⌘V: pane clipboard (copy here, paste in the other pane). Safe to
        // bind here because text fields/dialogs already returned above.
        if event.modifierFlags.contains(.command), !event.modifierFlags.contains(.option) {
            switch event.charactersIgnoringModifiers {
            case "c": state.copySelection(from: pane); return true
            case "v": state.paste(into: pane); return true
            case "l": state.focusPathBar = pane; return true
            default: break
            }
        }
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
        case 99:  // F3 — view file
            if let entry = state.selectedEntries(in: pane).first {
                state.viewFile(entry, pane: pane)
            }
            return true
        case 36:  // Return — open
            if let entry = state.selectedEntries(in: pane).first {
                if pane == .local { state.openLocal(entry) } else { state.openRemote(entry) }
            }
            return true
        default:
            return typeAheadJump(event, pane: pane)
        }
    }

    /// WinSCP type-ahead: typed letters select the first entry whose name
    /// starts with them; the buffer resets after a second of silence.
    private func typeAheadJump(_ event: NSEvent, pane: PaneKind) -> Bool {
        guard !event.modifierFlags.contains(.command),
            !event.modifierFlags.contains(.control),
            let chars = event.characters?.lowercased(), !chars.isEmpty,
            chars.unicodeScalars.allSatisfy({
                CharacterSet.alphanumerics.contains($0) || ".-_ ".unicodeScalars.contains($0)
            })
        else { return false }
        let now = Date()
        typeAhead = now.timeIntervalSince(typeAheadAt) < 1.0 ? typeAhead + chars : chars
        typeAheadAt = now
        let entries = (pane == .local ? state.localEntries : state.remoteEntries)
            .filter { state.showHidden || !$0.name.hasPrefix(".") }
        guard let hit = entries.first(where: { $0.name.lowercased().hasPrefix(typeAhead) })
        else { return true }
        if pane == .local {
            state.localSelection = [hit.id]
        } else {
            state.remoteSelection = [hit.id]
        }
        return true
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
                    Divider()
                    Toggle("Mirror mode (delete extraneous)", isOn: $state.mirrorSync)
                } label: {
                    Label("Synchronize", systemImage: "arrow.triangle.2.circlepath")
                }
                .menuStyle(.borderlessButton)
                .fixedSize()

                if state.proto == .sftp {
                    Button {
                        state.beginExecCommand()
                    } label: {
                        Image(systemName: "terminal.fill")
                    }
                    .help("Execute remote command (SFTP)")
                }

                Button {
                    state.showFind = true
                } label: {
                    Image(systemName: "magnifyingglass")
                }
                .help("Find remote files (mask, e.g. *.log)")

                Button {
                    state.openTerminal()
                } label: {
                    Image(systemName: "terminal")
                }
                .help("Open SSH session in Terminal")

                TextField("exclude: *.tmp; .git/", text: $state.excludeMasks)
                    .textFieldStyle(.roundedBorder)
                    .frame(width: 170)
                    .help("Exclusion masks for folder transfers and sync")
            }

            if state.busy { ProgressView().scaleEffect(0.6).frame(width: 18, height: 18) }
            Spacer()
            Text("F5 copy · F6 move · F2 rename · Tab panes")
                .font(.caption2)
                .foregroundStyle(.tertiary)
            Button {
                HelpWindowController.shared.show()
            } label: {
                Image(systemName: "questionmark.circle")
            }
            .help("Help")
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 6)
    }
}

// MARK: - Login dialog (WinSCP-style)

private struct LoginSheet: View {
    @EnvironmentObject var state: AppState
    @State private var showKnownHosts = false

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
                    Button("Import from ~/.ssh/config") { state.importSshConfig() }
                    Button("Export sites…") { state.exportSites() }
                    Divider()
                    Button("Manage known hosts…") { showKnownHosts = true }
                }
                .menuStyle(.borderlessButton)
                .fixedSize()
                Button("Save site…") { state.beginSaveSite() }
                    .disabled(state.host.isEmpty && state.bucket.isEmpty)
                Button { HelpWindowController.shared.show() } label: {
                    Image(systemName: "questionmark.circle")
                }
                .help("Help")
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
        .sheet(isPresented: $showKnownHosts) {
            KnownHostsSheet { showKnownHosts = false }
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

/// Lists SCP Commander's trusted SSH host keys with a "Forget" action, so a
/// changed key can be cleared instead of failing closed forever.
private struct KnownHostsSheet: View {
    let onClose: () -> Void
    @State private var hosts: [CoreClient.KnownHostInfo] = []

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Trusted host keys").font(.headline)
            Text(
                "Keys SCP Commander has accepted. Forget one to be re-prompted on "
                    + "the next connection. Your system ~/.ssh/known_hosts is not shown "
                    + "or modified here.")
                .font(.caption).foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            if hosts.isEmpty {
                Text("No trusted keys yet.")
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, minHeight: 120, alignment: .center)
            } else {
                List(hosts) { h in
                    HStack {
                        VStack(alignment: .leading, spacing: 1) {
                            Text(h.host)
                            Text(h.key_type).font(.caption).foregroundStyle(.secondary)
                        }
                        Spacer()
                        Button("Forget") {
                            CoreClient.removeKnownHost(h.host)
                            reload()
                        }
                    }
                }
                .frame(minHeight: 180)
            }
            HStack {
                Spacer()
                Button("Close") { onClose() }.keyboardShortcut(.defaultAction)
            }
        }
        .padding(16)
        .frame(width: 440)
        .onAppear { reload() }
    }

    private func reload() { hosts = CoreClient.listKnownHosts() }
}

/// Manage and run custom remote command templates. "{}" expands to the
/// selected remote file paths; Run executes against the current selection.
private struct CustomCommandsSheet: View {
    @EnvironmentObject var state: AppState
    let onClose: () -> Void
    @State private var newName = ""
    @State private var newTemplate = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Custom commands").font(.headline)
            Text("Run a templated command on the selected remote file(s). \u{201C}{}\u{201D} expands to their shell-quoted paths.")
                .font(.caption).foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            if state.customCommands.isEmpty {
                Text("No custom commands yet.")
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, minHeight: 80, alignment: .center)
            } else {
                List {
                    ForEach(state.customCommands) { cmd in
                        HStack {
                            VStack(alignment: .leading, spacing: 1) {
                                Text(cmd.name)
                                Text(cmd.template).font(.caption).foregroundStyle(.secondary)
                                    .lineLimit(1)
                            }
                            Spacer()
                            Button("Run") {
                                state.runCustomCommand(cmd)
                                onClose()
                            }
                            .disabled(state.proto != .sftp || !state.isConnected)
                            Button(role: .destructive) { state.removeCustomCommand(cmd) } label: {
                                Image(systemName: "trash")
                            }
                        }
                    }
                }
                .frame(minHeight: 150)
            }
            Divider()
            HStack {
                TextField("Name", text: $newName).frame(width: 120)
                TextField("Command (use {} for files)", text: $newTemplate)
                Button("Add") {
                    state.addCustomCommand(name: newName, template: newTemplate)
                    newName = ""; newTemplate = ""
                }
                .disabled(newName.isEmpty || newTemplate.isEmpty)
            }
            HStack {
                Spacer()
                Button("Close") { onClose() }.keyboardShortcut(.defaultAction)
            }
        }
        .padding(16)
        .frame(width: 480)
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
                            .onChange(of: state.host) { _ in state.tryFillSavedPassword() }
                        Text("Port:").foregroundStyle(.secondary)
                        TextField("port", text: $state.port).frame(width: 56)
                            .onChange(of: state.port) { _ in state.tryFillSavedPassword() }
                    }
                }
                GridRow {
                    Text(isS3 ? "Access key:" : "User name:")
                    TextField("", text: $state.user).frame(width: 180)
                        .onChange(of: state.user) { _ in state.tryFillSavedPassword() }
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
                if !(isSftp && state.authMode == .agent) {
                    GridRow {
                        Text("")
                        Toggle("Remember password", isOn: $state.rememberPassword)
                            .disabled(state.password.isEmpty)
                    }
                }
            }
            if isSftp {
                Divider().padding(.vertical, 2)
                Toggle("Connect through a jump host (bastion)", isOn: $state.useJump)
                if state.useJump {
                    Grid(alignment: .leading, horizontalSpacing: 10, verticalSpacing: 8) {
                        GridRow {
                            Text("Jump host:")
                            TextField("bastion.example.com", text: $state.jumpHost)
                        }
                        GridRow {
                            Text("Jump port:")
                            TextField("22", text: $state.jumpPort).frame(width: 80)
                        }
                        GridRow {
                            Text("Jump user:")
                            TextField("user", text: $state.jumpUser)
                        }
                        GridRow {
                            Text("Jump auth:")
                            Picker("", selection: $state.jumpAuthMode) {
                                ForEach(AuthMode.allCases, id: \.self) { Text($0.label).tag($0) }
                            }
                            .labelsHidden().frame(width: 160)
                        }
                        if state.jumpAuthMode == .keyFile {
                            GridRow {
                                Text("Jump key:")
                                TextField("~/.ssh/id_ed25519", text: $state.jumpKeyPath)
                            }
                        } else if state.jumpAuthMode != .agent {
                            GridRow {
                                Text("Jump password:")
                                SecureField("", text: $state.jumpPassword)
                            }
                        }
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
            } else if state.proto == .s3 && state.host.hasPrefix("http://") {
                Label(
                    "http:// endpoint sends credentials and data unencrypted.",
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
    let onCopyURL: ((FileEntry) -> Void)?
    let onProperties: (FileEntry) -> Void
    let onExternalDrop: (([URL]) -> Void)?
    var onCopyFile: ((FileEntry) -> Void)? = nil
    var onExec: ((FileEntry) -> Void)? = nil
    var onView: ((FileEntry) -> Void)? = nil
    var onClipboardCopy: (() -> Void)? = nil
    var onClipboardPaste: (() -> Void)? = nil
    var canPaste: Bool = false
    // Navigation history (WinSCP back/forward/home)
    var onBack: (() -> Void)? = nil
    var onForward: (() -> Void)? = nil
    var onHome: (() -> Void)? = nil
    var canBack: Bool = false
    var canForward: Bool = false
    /// Recent directories for this pane's dropdown (newest first).
    var recents: [String] = []
    /// Flips true to request keyboard focus on the path bar (⌘L).
    var focusRequested: Bool = false
    var onFocusHandled: (() -> Void)? = nil

    @State private var sortKey: SortKey = .name
    @State private var ascending = true
    @State private var pathText = ""
    @FocusState private var pathFocused: Bool
    @State private var filterText = ""
    // Resizable column widths (live values; persisted to UserDefaults on release).
    @State private var colWidths: [String: CGFloat] = [:]
    @State private var dragStartWidth: [String: CGFloat] = [:]
    /// Rendered width of the flexible Name column — drag base when first resized.
    @State private var nameMeasuredWidth: CGFloat = 200
    /// Rendered width of the whole header row — drag clamp + sanity check.
    @State private var headerWidth: CGFloat = 0

    /// Memoized sorted/filtered rows. Recomputed only when an input changes
    /// (see `recomputeDisplay`), never per render — large directories would
    /// otherwise re-sort on every SwiftUI pass and stutter.
    @State private var display: [FileEntry] = []

    /// Pure sort+filter, safe to run off the main thread.
    private static func sortFilter(
        _ entries: [FileEntry], showHidden: Bool, filter: String,
        key: SortKey, ascending: Bool
    ) -> [FileEntry] {
        var v = showHidden ? entries : entries.filter { !$0.name.hasPrefix(".") }
        if !filter.isEmpty {
            v = v.filter { $0.name.localizedCaseInsensitiveContains(filter) }
        }
        v.sort { a, b in
            if a.isDir != b.isDir { return a.isDir }
            let result: Bool
            switch key {
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
        return v
    }

    /// Recompute `display` from the current inputs. Big lists sort on a
    /// background queue so the UI never blocks; small lists stay synchronous
    /// to avoid a frame of flicker.
    private func recomputeDisplay() {
        let input = entries
        let hidden = showHidden
        let filter = filterText
        let key = sortKey
        let asc = ascending
        if input.count > 1500 {
            DispatchQueue.global(qos: .userInitiated).async {
                let result = Self.sortFilter(
                    input, showHidden: hidden, filter: filter, key: key, ascending: asc)
                DispatchQueue.main.async { self.display = result }
            }
        } else {
            display = Self.sortFilter(
                input, showHidden: hidden, filter: filter, key: key, ascending: asc)
        }
    }

    private var selectedEntries: [FileEntry] {
        display.filter { selection.contains($0.id) }
    }

    /// The clicked row plus the rest of the selection when it's part of it.
    private func batchTargets(_ entry: FileEntry) -> [FileEntry] {
        selection.contains(entry.id) ? selectedEntries : [entry]
    }

    var body: some View {
        VStack(spacing: 0) {
            paneToolbar
            Divider()
            columnHeader
            Divider()
            List(selection: $selection) {
                parentRow
                ForEach(display) { entry in
                    row(for: entry)
                }
            }
            .listStyle(.inset(alternatesRowBackgrounds: true))
            .background(DoubleClickMonitor {
                if selection.contains("..") {
                    onUp()
                } else if let entry = display.first(where: { selection.contains($0.id) }) {
                    onOpen(entry)
                }
            })
            .onChange(of: selection) { _ in onFocus() }
            .onAppear { recomputeDisplay() }
            .onChange(of: entries) { _ in recomputeDisplay() }
            .onChange(of: showHidden) { _ in recomputeDisplay() }
            .onChange(of: filterText) { _ in recomputeDisplay() }
            .onChange(of: sortKey) { _ in recomputeDisplay() }
            .onChange(of: ascending) { _ in recomputeDisplay() }
            .dropDestination(for: DraggedFile.self) { items, _ in
                for file in items where file.pane != kind { onDrop(file) }
                return items.contains { $0.pane != kind }
            }
            .dropDestination(for: URL.self) { urls, _ in
                guard let onExternalDrop, !urls.isEmpty else { return false }
                onExternalDrop(urls)
                return true
            }
            Divider()
            selectionFooter
        }
        .frame(minWidth: 380)
        .contentShape(Rectangle())
        .onTapGesture { onFocus() }
    }

    /// WinSCP-style status line: total items, or the size of the selection.
    private var selectionFooter: some View {
        let sel = selectedEntries
        let bytes = sel.filter { !$0.isDir }.reduce(UInt64(0)) { $0 + $1.size }
        return HStack(spacing: 5) {
            if sel.isEmpty {
                Text("\(display.count) item\(display.count == 1 ? "" : "s")")
            } else {
                Text("\(sel.count) of \(display.count) selected")
                if bytes > 0 {
                    Text("·")
                    Text(humanSize(bytes))
                }
            }
            Spacer()
        }
        .font(.caption2)
        .foregroundStyle(.secondary)
        .lineLimit(1)
        .padding(.horizontal, 8)
        .padding(.vertical, 3)
    }

    /// WinSCP-style per-pane header: title + nav buttons, then address bar.
    private var paneToolbar: some View {
        VStack(spacing: 0) {
            // ── Row 1: title + action buttons ───────────────────────────────
            HStack(spacing: 2) {
                Text(title)
                    .font(.subheadline.bold())
                    .foregroundStyle(isFocused ? Color.accentColor : Color.primary)
                    .padding(.trailing, 2)
                if let onBack {
                    toolButton("chevron.left", "Back", disabled: !canBack, action: onBack)
                }
                if let onForward {
                    toolButton("chevron.right", "Forward", disabled: !canForward, action: onForward)
                }
                toolButton("arrow.up", "Parent directory", action: onUp)
                if let onHome {
                    toolButton("house", "Home directory", action: onHome)
                }
                toolButton("arrow.clockwise", "Refresh", action: onRefresh)
                bookmarkMenu
                Divider().frame(height: 14).padding(.horizontal, 2)
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
                toolButton("info.circle", "Properties (F9)", disabled: selectedEntries.isEmpty) {
                    if let e = selectedEntries.first { onProperties(e) }
                }
                Spacer()
                TextField("filter", text: $filterText)
                    .textFieldStyle(.roundedBorder)
                    .font(.caption)
                    .frame(width: 86)
                    .help("Filter the visible listing by name")
            }
            .padding(.horizontal, 8)
            .padding(.top, 4)
            .padding(.bottom, 3)

            // ── Row 2: address bar ──────────────────────────────────────────
            HStack(spacing: 4) {
                Image(systemName: "folder")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                TextField("path", text: $pathText)
                    .textFieldStyle(.plain)
                    .font(.system(size: 12, design: .monospaced))
                    .focused($pathFocused)
                    .onAppear { pathText = path }
                    .onChange(of: path) { pathText = $0 }
                    .onSubmit { onNavigate(pathText) }
                    .onChange(of: focusRequested) {
                        if $0 { pathFocused = true; onFocusHandled?() }
                    }
                if !recents.isEmpty {
                    Menu {
                        ForEach(recents, id: \.self) { dir in
                            Button(dir) { onNavigate(dir) }
                        }
                    } label: {
                        Image(systemName: "clock.arrow.circlepath").font(.caption)
                    }
                    .menuStyle(.borderlessButton)
                    .menuIndicator(.hidden)
                    .fixedSize()
                    .help("Recent locations")
                }
            }
            .padding(.horizontal, 8)
            .padding(.vertical, 4)
            .background(.background.opacity(0.6))
            .overlay(
                RoundedRectangle(cornerRadius: 5)
                    .stroke(isFocused ? Color.accentColor.opacity(0.4) : Color.secondary.opacity(0.25), lineWidth: 1)
                    .padding(.horizontal, 6)
            )
            .padding(.bottom, 3)
        }
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

    // MARK: Directory bookmarks (persisted per pane)

    private var bookmarksKey: String { "bookmarks.\(kind)" }

    private var bookmarkMenu: some View {
        Menu {
            let bookmarks = UserDefaults.standard.stringArray(forKey: bookmarksKey) ?? []
            if bookmarks.isEmpty {
                Text("No bookmarks")
            }
            ForEach(bookmarks, id: \.self) { b in
                Button(b) { onNavigate(b) }
            }
            Divider()
            if bookmarks.contains(path) {
                Button("Remove Bookmark") {
                    UserDefaults.standard.set(
                        bookmarks.filter { $0 != path }, forKey: bookmarksKey)
                }
            } else {
                Button("Bookmark This Directory") {
                    UserDefaults.standard.set(bookmarks + [path], forKey: bookmarksKey)
                }
            }
        } label: {
            Image(systemName: "star")
        }
        .menuStyle(.borderlessButton)
        .fixedSize()
        .help("Directory bookmarks")
    }

    // MARK: Resizable columns

    private static let defaultWidths: [String: CGFloat] = [
        "size": 76, "type": 110, "changed": 118, "owner": 48, "group": 48, "rights": 80,
    ]

    /// Current width of a column: live drag value, else persisted, else default.
    private func colWidth(_ col: String) -> CGFloat {
        if let w = colWidths[col] { return w }
        let saved = UserDefaults.standard.double(forKey: "colw.\(kind).\(col)")
        return saved >= 40 ? CGFloat(saved) : Self.defaultWidths[col, default: 80]
    }

    /// Name is flexible until the user drags its divider; then it's fixed.
    private var nameWidth: CGFloat? {
        if let w = colWidths["name"] { return w }
        let saved = UserDefaults.standard.double(forKey: "colw.\(kind).name")
        return saved >= 80 ? CGFloat(saved) : nil
    }

    /// Fixed-width columns present in this pane.
    private var fixedColumns: [String] {
        showRights
            ? ["size", "type", "changed", "owner", "group", "rights"]
            : ["size", "type", "changed"]
    }

    /// Widest this column may get without pushing the others (or Name's
    /// 80pt minimum) past the pane edge.
    private func maxColWidth(_ col: String) -> CGFloat {
        guard headerWidth > 0 else { return col == "name" ? 800 : 400 }
        let others = fixedColumns.filter { $0 != col }.map(colWidth).reduce(0, +)
        let nameMin: CGFloat = col == "name" ? 0 : 80
        let handles = CGFloat(fixedColumns.count + 1) * 7
        return max(40, headerWidth - others - nameMin - handles - 24)
    }

    /// Thin draggable divider that resizes the column to its left.
    /// Double-click resets the column to its default width.
    private func resizeHandle(_ col: String) -> some View {
        Rectangle()
            .fill(Color.secondary.opacity(0.3))
            .frame(width: 1, height: 12)
            .padding(.horizontal, 3)
            .contentShape(Rectangle().inset(by: -2))
            .onHover { inside in
                if inside { NSCursor.resizeLeftRight.push() } else { NSCursor.pop() }
            }
            .gesture(
                DragGesture(minimumDistance: 1)
                    .onChanged { v in
                        if dragStartWidth[col] == nil {
                            dragStartWidth[col] =
                                col == "name" ? (nameWidth ?? nameMeasuredWidth) : colWidth(col)
                        }
                        let minW: CGFloat = col == "name" ? 80 : 40
                        var desired = max(minW, dragStartWidth[col]! + v.translation.width)
                        if col == "name" {
                            // Widening Name steals space from the other columns,
                            // shrinking them (down to 40pt each) to make room.
                            let handles = CGFloat(fixedColumns.count + 1) * 7
                            let budget = headerWidth - handles - 24
                            let othersMin = CGFloat(fixedColumns.count) * 40
                            desired = min(desired, budget - othersMin)
                            let others = fixedColumns.map(colWidth).reduce(0, +)
                            if desired + others > budget, others > 0 {
                                let scale = (budget - desired) / others
                                for c in fixedColumns {
                                    colWidths[c] = max(40, colWidth(c) * scale)
                                }
                            }
                            colWidths[col] = desired
                        } else {
                            colWidths[col] = min(maxColWidth(col), desired)
                        }
                    }
                    .onEnded { _ in
                        dragStartWidth[col] = nil
                        // Persist every column the drag touched (a Name drag
                        // may have shrunk the others too).
                        for (c, w) in colWidths {
                            UserDefaults.standard.set(Double(w), forKey: "colw.\(kind).\(c)")
                        }
                    }
            )
            .onTapGesture(count: 2) {
                colWidths[col] = nil
                UserDefaults.standard.removeObject(forKey: "colw.\(kind).\(col)")
            }
    }

    /// Saved widths that no longer fit this pane reset to defaults — columns
    /// must never push Name off the edge.
    private func sanitizeColumnWidths(paneWidth: CGFloat) {
        guard paneWidth > 0 else { return }
        let handles = CGFloat(fixedColumns.count + 1) * 7
        let fixed = fixedColumns.map(colWidth).reduce(0, +)
        let name = nameWidth ?? 80
        if name + fixed + handles + 24 > paneWidth {
            for col in fixedColumns + ["name"] {
                colWidths[col] = nil
                UserDefaults.standard.removeObject(forKey: "colw.\(kind).\(col)")
            }
        }
    }

    /// Clickable, sortable column headers with drag-to-resize dividers.
    private var columnHeader: some View {
        HStack(spacing: 0) {
            headerCell("Name", key: .name, alignment: .leading)
                .frame(
                    minWidth: nameWidth ?? 80,
                    maxWidth: nameWidth ?? .infinity,
                    alignment: .leading)
                .layoutPriority(1)  // never let fixed columns squeeze Name away
                .background(
                    GeometryReader { g in
                        Color.clear.onAppear { nameMeasuredWidth = g.size.width }
                            .onChange(of: g.size.width) { nameMeasuredWidth = $0 }
                    })
            resizeHandle("name")
            headerCell("Size", key: .size, alignment: .trailing)
                .frame(width: colWidth("size"), alignment: .trailing)
            resizeHandle("size")
            headerCell("Type", key: .type, alignment: .leading)
                .frame(width: colWidth("type"), alignment: .leading)
            resizeHandle("type")
            headerCell("Changed", key: .mtime, alignment: .leading)
                .frame(width: colWidth("changed"), alignment: .leading)
            resizeHandle("changed")
            if showRights {
                Text("Owner")
                    .font(.caption.bold())
                    .foregroundStyle(.secondary)
                    .frame(width: colWidth("owner"), alignment: .trailing)
                resizeHandle("owner")
                Text("Group")
                    .font(.caption.bold())
                    .foregroundStyle(.secondary)
                    .frame(width: colWidth("group"), alignment: .trailing)
                resizeHandle("group")
                Text("Rights")
                    .font(.caption.bold())
                    .foregroundStyle(.secondary)
                    .frame(width: colWidth("rights"), alignment: .leading)
                resizeHandle("rights")
            }
            if nameWidth != nil { Spacer(minLength: 0) }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, 12)
        .padding(.vertical, 3)
        .background(
            GeometryReader { g in
                Color.clear
                    .onAppear {
                        headerWidth = g.size.width
                        sanitizeColumnWidths(paneWidth: g.size.width)
                    }
                    .onChange(of: g.size.width) { headerWidth = $0 }
            })
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

    /// Fixed ".." row at the top of every listing — double-click navigates up.
    private var parentRow: some View {
        HStack(spacing: 6) {
            Image(systemName: "arrow.up.left").frame(width: 16)
            Text("..").bold()
            Spacer()
        }
        .foregroundStyle(.secondary)
        .tag("..")
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
            .frame(
                minWidth: nameWidth ?? 80,
                maxWidth: nameWidth ?? .infinity,
                alignment: .leading)
            .layoutPriority(1)
            Color.clear.frame(width: 7)  // aligns with the header resize handle
            Text(entry.isDir ? "" : humanSize(entry.size))
                .font(.caption.monospacedDigit())
                .foregroundStyle(.secondary)
                .frame(width: colWidth("size"), alignment: .trailing)
            Color.clear.frame(width: 7)
            Text(entry.typeDescription)
                .font(.caption)
                .foregroundStyle(.secondary)
                .lineLimit(1)
                .frame(width: colWidth("type"), alignment: .leading)
            Color.clear.frame(width: 7)
            Text(entry.mtime.map { changedFormatter.string(from: $0) } ?? "")
                .font(.caption.monospacedDigit())
                .foregroundStyle(.secondary)
                .frame(width: colWidth("changed"), alignment: .leading)
            Color.clear.frame(width: 7)
            if showRights {
                Text(entry.uid.map { String($0) } ?? "")
                    .font(.caption.monospacedDigit())
                    .foregroundStyle(.secondary)
                    .frame(width: colWidth("owner"), alignment: .trailing)
                Color.clear.frame(width: 7)
                Text(entry.gid.map { String($0) } ?? "")
                    .font(.caption.monospacedDigit())
                    .foregroundStyle(.secondary)
                    .frame(width: colWidth("group"), alignment: .trailing)
                Color.clear.frame(width: 7)
                Text(entry.perms ?? "")
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
                    .frame(width: colWidth("rights"), alignment: .leading)
                Color.clear.frame(width: 7)
            }
            if nameWidth != nil { Spacer(minLength: 0) }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .tag(entry.id)
        .contentShape(Rectangle())
        .draggable({
            let sep = path.hasSuffix("/") ? "" : "/"
            return DraggedFile(
                pane: kind, name: entry.name, path: "\(path)\(sep)\(entry.name)",
                isDir: entry.isDir)
        }())
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
                if let onView {
                    Button("View (F3)") { onView(entry) }
                }
                if let onEdit {
                    Button("Edit (auto-upload on save)") { onEdit(entry) }
                }
                if let onCopyFile {
                    Button("Duplicate…") { onCopyFile(entry) }
                }
                if let onCopyURL {
                    Button("Copy URL") { onCopyURL(entry) }
                }
            }
            if let onExec {
                Button("Execute command…") { onExec(entry) }
            }
            if onClipboardCopy != nil || (canPaste && onClipboardPaste != nil) {
                Divider()
                if let onClipboardCopy {
                    Button("Copy") { onClipboardCopy() }
                }
                if canPaste, let onClipboardPaste {
                    Button("Paste") { onClipboardPaste() }
                }
            }
            Divider()
            Button("Copy Path") {
                let sep = path.hasSuffix("/") ? "" : "/"
                let full = "\(path)\(sep)\(entry.name)"
                NSPasteboard.general.clearContents()
                NSPasteboard.general.setString(full, forType: .string)
            }
            if kind == "local" {
                Button("Reveal in Finder") {
                    let sep = path.hasSuffix("/") ? "" : "/"
                    let full = "\(path)\(sep)\(entry.name)"
                    NSWorkspace.shared.activateFileViewerSelecting(
                        [URL(fileURLWithPath: full)])
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

// MARK: - Sync preview (WinSCP's synchronization checklist)

private struct SyncPreviewSheet: View {
    @EnvironmentObject var state: AppState
    @State private var selected: Set<String> = []
    @State private var loaded = false

    var body: some View {
        let preview = state.syncPreview
        VStack(alignment: .leading, spacing: 10) {
            Text(
                preview?.download == true
                    ? "Synchronize remote → local — preview"
                    : "Synchronize local → remote — preview"
            )
            .font(.headline)
            if let preview {
                HStack {
                    Text(
                        "\(preview.plan.items.count) file(s) to copy, \(preview.plan.dirs.count) folder(s) to create"
                        + (preview.plan.deletes.isEmpty ? "" : ", \(preview.plan.deletes.count) to delete")
                    )
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    Spacer()
                    Toggle("Mirror (delete extraneous)", isOn: $state.mirrorSync)
                        .toggleStyle(.checkbox)
                        .font(.caption)
                }
                List(preview.plan.items) { item in
                    Toggle(isOn: binding(for: item.rel)) {
                        HStack {
                            Text(item.rel).lineLimit(1)
                            Spacer()
                            Text("\(item.reason) · \(item.size) B")
                                .font(.caption.monospacedDigit())
                                .foregroundStyle(.secondary)
                        }
                    }
                }
                .frame(minHeight: 240)
            }
            HStack {
                Spacer()
                Button("Cancel") { state.syncPreview = nil }
                    .keyboardShortcut(.cancelAction)
                Button("Synchronize") {
                    guard let preview = state.syncPreview else { return }
                    let items = preview.plan.items.filter { selected.contains($0.rel) }
                    state.runSyncItems(items)
                }
                .keyboardShortcut(.defaultAction)
                .disabled(selected.isEmpty)
            }
        }
        .padding(14)
        .frame(width: 520, height: 400)
        .onAppear {
            guard !loaded, let preview = state.syncPreview else { return }
            loaded = true
            selected = Set(preview.plan.items.map(\.rel))
        }
    }

    private func binding(for rel: String) -> Binding<Bool> {
        Binding(
            get: { selected.contains(rel) },
            set: { on in
                if on { selected.insert(rel) } else { selected.remove(rel) }
            })
    }
}

// MARK: - Find files

private struct FindSheet: View {
    @EnvironmentObject var state: AppState
    @State private var mask = "*"

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Find files under \(state.remotePath)").font(.headline)
            HStack {
                TextField("mask, e.g. *.log", text: $mask)
                    .onSubmit { state.runFind(mask: mask) }
                Button("Search") { state.runFind(mask: mask) }
                    .keyboardShortcut(.defaultAction)
            }
            if let results = state.findResults {
                Text("\(results.hits.count) match(es) for \(results.mask)")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                List(results.hits) { hit in
                    HStack {
                        Image(systemName: hit.is_dir ? "folder" : "doc")
                            .foregroundStyle(.secondary)
                        Text(hit.path).lineLimit(1).truncationMode(.head)
                        Spacer()
                        Button("Open dir") {
                            let parent = (hit.path as NSString).deletingLastPathComponent
                            state.navigateRemote(parent.isEmpty ? "/" : parent)
                            state.showFind = false
                        }
                        .buttonStyle(.borderless)
                        .font(.caption)
                    }
                }
                .frame(minHeight: 220)
            } else {
                Spacer()
            }
            HStack {
                Spacer()
                Button("Close") { state.showFind = false }.keyboardShortcut(.cancelAction)
            }
        }
        .padding(14)
        .frame(width: 560, height: 420)
    }
}

// MARK: - Execute command dialogs

private struct ExecCommandSheet: View {
    @EnvironmentObject var state: AppState

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Execute command on \(state.host)").font(.headline)
            TextField("command, e.g. ls -la /tmp", text: $state.execCmd)
                .textFieldStyle(.roundedBorder)
                .onSubmit { state.runExecCommand() }
            HStack {
                Spacer()
                Button("Cancel") { state.showExecDialog = false }
                    .keyboardShortcut(.cancelAction)
                Button("Execute") { state.runExecCommand() }
                    .keyboardShortcut(.defaultAction)
                    .disabled(state.execCmd.isEmpty)
            }
        }
        .padding(16)
        .frame(width: 420)
    }
}

private struct ExecResultSheet: View {
    @EnvironmentObject var state: AppState

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            if let result = state.execResult {
                HStack {
                    Text("Command output").font(.headline)
                    Spacer()
                    Text("Exit \(result.exitCode)")
                        .font(.caption.monospaced())
                        .padding(.horizontal, 6).padding(.vertical, 2)
                        .background(
                            RoundedRectangle(cornerRadius: 4)
                                .fill(result.exitCode == 0 ? Color.green.opacity(0.15) : Color.red.opacity(0.15))
                        )
                        .foregroundStyle(result.exitCode == 0 ? .green : .red)
                }
                if !result.stdout.isEmpty {
                    Text("stdout").font(.caption.bold()).foregroundStyle(.secondary)
                    ScrollView {
                        Text(result.stdout)
                            .font(.caption.monospaced())
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .textSelection(.enabled)
                    }
                    .background(Color.secondary.opacity(0.08))
                    .clipShape(RoundedRectangle(cornerRadius: 4))
                    .frame(minHeight: 80, maxHeight: 220)
                }
                if !result.stderr.isEmpty {
                    Text("stderr").font(.caption.bold()).foregroundStyle(.red)
                    ScrollView {
                        Text(result.stderr)
                            .font(.caption.monospaced())
                            .foregroundStyle(.red)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .textSelection(.enabled)
                    }
                    .background(Color.red.opacity(0.07))
                    .clipShape(RoundedRectangle(cornerRadius: 4))
                    .frame(minHeight: 40, maxHeight: 120)
                }
            }
            HStack {
                Spacer()
                Button("Close") { state.showExecResult = false }
                    .keyboardShortcut(.cancelAction)
            }
        }
        .padding(14)
        .frame(width: 560)
        .frame(minHeight: 160)
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

// MARK: - Internal file viewer (F3)

struct ViewerSheet: View {
    let name: String
    let text: String
    let onClose: () -> Void

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Image(systemName: "doc.text.magnifyingglass")
                Text(name).font(.headline)
                Spacer()
                Button("Close") { onClose() }.keyboardShortcut(.escape)
            }
            .padding(12)
            Divider()
            ScrollView([.vertical, .horizontal]) {
                Text(text)
                    .font(.system(size: 12, design: .monospaced))
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(10)
            }
        }
        .frame(minWidth: 560, idealWidth: 700, minHeight: 380, idealHeight: 520)
    }
}

// MARK: - Bottom command bar

private struct CommandBar: View {
    @EnvironmentObject var state: AppState

    var body: some View {
        HStack(spacing: 6) {
            Menu {
                Button("Local → Remote (upload changes)") { state.sync(download: false) }
                Button("Remote → Local (download changes)") { state.sync(download: true) }
                Divider()
                Toggle("Mirror mode (delete extraneous)", isOn: $state.mirrorSync)
            } label: {
                Label("Synchronize", systemImage: "arrow.triangle.2.circlepath")
                    .font(.caption)
            }
            .menuStyle(.borderedButton)
            .controlSize(.small)
            .fixedSize()
            .disabled(!state.isConnected)

            Divider().frame(height: 16)

            Menu {
                Button("Show Queue") { TransferWindowController.shared.show(queue: state.transfers, state: state) }
                Divider()
                Button("Clear Finished") { state.transfers.clearFinished() }
                Button("Cancel All", role: .destructive) { state.transfers.cancelAll() }
            } label: {
                Label("Queue", systemImage: "list.bullet.rectangle")
                    .font(.caption)
            }
            .menuStyle(.borderedButton)
            .controlSize(.small)
            .fixedSize()

            Divider().frame(height: 16)

            Menu {
                Text("Transfer Settings").font(.caption).foregroundStyle(.secondary)
                Divider()
                Picker("Speed limit", selection: $state.speedLimitKbs) {
                    Text("No limit").tag(0)
                    Text("100 KiB/s").tag(100)
                    Text("500 KiB/s").tag(500)
                    Text("1 MiB/s").tag(1024)
                    Text("5 MiB/s").tag(5120)
                }
            } label: {
                Label(
                    state.speedLimitKbs == 0 ? "Transfer Settings: Default"
                        : "Transfer Settings: \(state.speedLimitKbs < 1024 ? "\(state.speedLimitKbs) KiB/s" : "\(state.speedLimitKbs / 1024) MiB/s")",
                    systemImage: "slider.horizontal.3"
                )
                .font(.caption)
            }
            .menuStyle(.borderedButton)
            .controlSize(.small)
            .fixedSize()

            Spacer()
        }
        .padding(.horizontal, 8).padding(.vertical, 4)
    }
}

// MARK: - Reconnect sheet

private struct ReconnectSheet: View {
    @EnvironmentObject var state: AppState

    var body: some View {
        VStack(spacing: 16) {
            HStack(spacing: 12) {
                Image(systemName: "exclamationmark.triangle.fill")
                    .font(.largeTitle)
                    .foregroundStyle(.red)
                VStack(alignment: .leading, spacing: 4) {
                    Text("Connection lost").font(.headline)
                    Text(state.reconnectMessage ?? "")
                        .font(.body)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            HStack(spacing: 12) {
                Button("Cancel") { state.dismissReconnect() }
                    .keyboardShortcut(.escape)
                Spacer()
                Button("Reconnect (\(state.reconnectCountdown) s)") { state.triggerReconnect() }
                    .buttonStyle(.borderedProminent)
                    .keyboardShortcut(.return)
            }
        }
        .padding(24)
        .frame(width: 420)
    }
}

func humanSize(_ bytes: UInt64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB"]
    var value = Double(bytes)
    var i = 0
    while value >= 1024 && i < units.count - 1 {
        value /= 1024
        i += 1
    }
    return i == 0 ? "\(bytes) B" : String(format: "%.1f %@", value, units[i])
}
