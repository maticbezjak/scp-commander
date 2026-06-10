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

struct ContentView: View {
    @EnvironmentObject var state: AppState

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
                        onDrop: { if $0.pane == "remote" { state.downloadByName($0.name) } }
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
                        onDrop: { if $0.pane == "local" { state.uploadByName($0.name) } }
                    )
                }
                TransfersPanel(queue: state.transfers)
                Divider()
                StatusBar()
            }
        }
        .frame(minWidth: 980, minHeight: 560)
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

// MARK: - Sites sidebar

private struct SitesSidebar: View {
    @EnvironmentObject var state: AppState
    @ObservedObject var store: SitesStore

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                Text("Sites").font(.headline)
                Spacer()
                Button(action: { state.saveCurrentSite() }) {
                    Image(systemName: "plus")
                }
                .buttonStyle(.borderless)
                .help("Save current connection")
            }
            .padding(8)
            Divider()

            List {
                ForEach(store.sites) { site in
                    HStack(spacing: 6) {
                        Image(systemName: "bookmark.fill").foregroundStyle(.tint)
                        VStack(alignment: .leading, spacing: 1) {
                            Text(site.name).lineLimit(1)
                            Text(site.proto.label).font(.caption2).foregroundStyle(.secondary)
                        }
                    }
                    .contentShape(Rectangle())
                    .onTapGesture { state.loadSite(site) }
                    .contextMenu {
                        Button("Load") { state.loadSite(site) }
                        Button("Delete", role: .destructive) { store.remove(site) }
                    }
                }
            }
            .listStyle(.sidebar)
        }
    }
}

// MARK: - Connection bar

private struct ConnectionBar: View {
    @EnvironmentObject var state: AppState

    var body: some View {
        HStack(spacing: 8) {
            Picker("", selection: $state.proto) {
                ForEach(Proto.allCases, id: \.self) { Text($0.label).tag($0) }
            }
            .labelsHidden()
            .frame(width: 80)

            let isS3 = state.proto == .s3
            TextField(isS3 ? "access key" : "user", text: $state.user).frame(width: 110)
            Text("@").foregroundStyle(.secondary)
            TextField(isS3 ? "endpoint (blank = AWS)" : "host", text: $state.host)
                .frame(minWidth: 140)
            TextField("port", text: $state.port).frame(width: 56)
            SecureField(isS3 ? "secret key" : "password", text: $state.password)
                .frame(width: 140)
            if isS3 {
                TextField("bucket", text: $state.bucket).frame(width: 100)
                TextField("region", text: $state.region).frame(width: 90)
            }

            Button(action: { state.connect() }) {
                Text(state.isConnected ? "Reconnect" : "Connect")
            }
            .keyboardShortcut(.return, modifiers: [])
            .disabled(state.busy || (isS3 ? state.bucket.isEmpty : state.host.isEmpty))

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

    @State private var selection: FileEntry.ID?

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Text(title).font(.headline)
                Spacer()
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
                    row(for: entry)
                }
            }
            .listStyle(.inset(alternatesRowBackgrounds: true))
            .dropDestination(for: DraggedFile.self) { items, _ in
                for file in items { onDrop(file) }
                return !items.isEmpty
            }
        }
        .frame(minWidth: 360)
    }

    @ViewBuilder
    private func row(for entry: FileEntry) -> some View {
        let base = EntryRow(entry: entry)
            .tag(entry.id)
            .contentShape(Rectangle())
            .onTapGesture(count: 2) { onOpen(entry) }
            .contextMenu {
                if entry.isDir {
                    Button("Open") { onOpen(entry) }
                } else {
                    Button(transferLabel) { onTransfer(entry) }
                }
            }

        if entry.isDir {
            base
        } else {
            base.draggable(DraggedFile(pane: kind, name: entry.name))
        }
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
                .frame(maxHeight: 110)
            }
        }
    }
}

private struct TransferRow: View {
    @ObservedObject var transfer: Transfer

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: transfer.direction.symbol)
            Text(transfer.name).lineLimit(1).frame(width: 160, alignment: .leading)
            progress
            Spacer()
            Text(detail).font(.caption.monospacedDigit()).foregroundStyle(.secondary)
        }
    }

    @ViewBuilder
    private var progress: some View {
        switch transfer.state {
        case .active:
            if let f = transfer.fraction {
                ProgressView(value: f).frame(width: 180)
            } else {
                ProgressView().scaleEffect(0.5).frame(width: 40)
            }
        case .done:
            Image(systemName: "checkmark.circle.fill").foregroundStyle(.green)
        case .failed:
            Image(systemName: "xmark.octagon.fill").foregroundStyle(.red)
        }
    }

    private var detail: String {
        switch transfer.state {
        case .failed(let msg): return msg
        case .done: return humanSize(transfer.total > 0 ? transfer.total : transfer.transferred)
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
