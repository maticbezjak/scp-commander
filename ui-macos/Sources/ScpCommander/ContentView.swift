import SwiftUI

struct ContentView: View {
    @EnvironmentObject var state: AppState

    var body: some View {
        VStack(spacing: 0) {
            ConnectionBar()
            Divider()
            HSplitView {
                FilePane(
                    title: "Local",
                    path: state.localPath,
                    entries: state.localEntries,
                    onUp: { state.localUp() },
                    onOpen: { state.openLocal($0) },
                    transferLabel: "Upload →",
                    onTransfer: { state.upload($0) }
                )
                FilePane(
                    title: "Remote",
                    path: state.remotePath,
                    entries: state.remoteEntries,
                    onUp: { state.remoteUp() },
                    onOpen: { state.openRemote($0) },
                    transferLabel: "← Download",
                    onTransfer: { state.download($0) }
                )
            }
            Divider()
            StatusBar()
        }
        .frame(minWidth: 820, minHeight: 520)
    }
}

private struct ConnectionBar: View {
    @EnvironmentObject var state: AppState

    var body: some View {
        HStack(spacing: 8) {
            Picker("", selection: $state.proto) {
                Text("SFTP").tag(Proto.sftp)
                Text("FTP").tag(Proto.ftp)
                Text("FTPS").tag(Proto.ftps)
                Text("S3").tag(Proto.s3)
            }
            .labelsHidden()
            .frame(width: 80)

            TextField("user", text: $state.user).frame(width: 110)
            Text("@").foregroundStyle(.secondary)
            TextField("host", text: $state.host).frame(minWidth: 140)
            TextField("port", text: $state.port).frame(width: 56)
            SecureField("password", text: $state.password).frame(width: 140)

            Button(action: { state.connect() }) {
                Text(state.isConnected ? "Reconnect" : "Connect")
            }
            .keyboardShortcut(.return, modifiers: [])
            .disabled(state.busy || state.host.isEmpty)

            if state.busy { ProgressView().scaleEffect(0.6).frame(width: 18, height: 18) }
            Spacer()
        }
        .padding(8)
    }
}

private struct FilePane: View {
    let title: String
    let path: String
    let entries: [FileEntry]
    let onUp: () -> Void
    let onOpen: (FileEntry) -> Void
    let transferLabel: String
    let onTransfer: (FileEntry) -> Void

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
                    EntryRow(entry: entry)
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
                }
            }
            .listStyle(.inset(alternatesRowBackgrounds: true))
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
