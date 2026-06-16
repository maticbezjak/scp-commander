import AppKit
import CScpCore
import SwiftUI

/// The standard ⌘, Preferences window: settings that otherwise live in
/// scattered toolbars, plus a few that were previously hardcoded.
struct PreferencesView: View {
    @EnvironmentObject var state: AppState
    @AppStorage("editorPath") private var editorPath = ""
    @AppStorage("transferPoolSize") private var poolSize = 3
    @AppStorage("keepaliveSeconds") private var keepaliveSeconds = 30
    @AppStorage("atomicUploads") private var atomicUploads = true

    var body: some View {
        Form {
            Section("Editing") {
                HStack {
                    TextField("Editor app", text: $editorPath)
                        .truncationMode(.head)
                    Button("Choose…") { chooseEditor() }
                    if !editorPath.isEmpty {
                        Button("Default") { editorPath = "" }
                    }
                }
                Text("App used for \u{201C}Edit\u{201D} on remote files. Empty = the system default for each file type.")
                    .font(.caption).foregroundStyle(.secondary)
            }
            Section("Transfers") {
                Stepper("Parallel connections: \(poolSize)", value: $poolSize, in: 1...8)
                Text("Applies to sessions connected after this change.")
                    .font(.caption).foregroundStyle(.secondary)
                TextField("Default exclude masks", text: $state.excludeMasks)
                Text("e.g. *.tmp; .git/ — skipped during folder transfers and sync.")
                    .font(.caption).foregroundStyle(.secondary)
                Toggle("Upload to temporary file first", isOn: $atomicUploads)
                    .onChange(of: atomicUploads) { scp_set_atomic_uploads($0 ? 1 : 0) }
                Text("Uploads land under a temp name and rename on success, so an interrupted transfer never leaves a truncated file.")
                    .font(.caption).foregroundStyle(.secondary)
            }
            Section("Connection") {
                Stepper("Keepalive every \(keepaliveSeconds)s", value: $keepaliveSeconds, in: 10...300, step: 5)
                    .onChange(of: keepaliveSeconds) { _ in state.restartKeepalive() }
                Text("How often idle sessions send a NAT keepalive.")
                    .font(.caption).foregroundStyle(.secondary)
            }
        }
        .formStyle(.grouped)
        .frame(width: 460)
    }

    private func chooseEditor() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        panel.allowedContentTypes = [.application]
        panel.directoryURL = URL(fileURLWithPath: "/Applications")
        if panel.runModal() == .OK, let url = panel.url { editorPath = url.path }
    }
}
