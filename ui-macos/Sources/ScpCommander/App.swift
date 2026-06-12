import SwiftUI

@main
struct ScpCommanderApp: App {
    @StateObject private var state = AppState()

    var body: some Scene {
        WindowGroup("SCP Commander") {
            ContentView()
                .environmentObject(state)
                .onOpenURL { url in
                    Task { @MainActor in state.openURL(url) }
                }
        }
        .windowStyle(.titleBar)
        .commands {
            // WinSCP menu layout: Left, Mark, Files, Commands, Tabs, Options, Right, Help
            CommandMenu("Left") {
                paneMenu(.local)
            }
            CommandMenu("Mark") {
                Button("Select All") { state.selectAll() }
                    .keyboardShortcut("a", modifiers: [.command, .shift])
                Button("Unselect All") { state.unselectAll() }
                    .keyboardShortcut("d", modifiers: [.command, .shift])
                Button("Invert Selection") { state.invertSelection() }
                    .keyboardShortcut("i", modifiers: [.command, .shift])
            }
            CommandMenu("Files") {
                Button("Transfer (F5)") { state.transferSelected() }
                    .keyboardShortcut("t", modifiers: .command)
                Divider()
                Button("Rename (F2)") { state.pendingMenuAction = .rename }
                Button("Duplicate (Shift+F5)") { state.pendingMenuAction = .duplicate }
                Button("Delete (F8)") { state.pendingMenuAction = .delete }
                    .keyboardShortcut(.delete, modifiers: .command)
                Divider()
                Button("New Folder (F7)") { state.pendingMenuAction = .newFolder }
                    .keyboardShortcut("n", modifiers: [.command, .shift])
                Button("Properties (F9)") { state.pendingMenuAction = .properties }
                    .keyboardShortcut("i", modifiers: .command)
            }
            CommandMenu("Commands") {
                Button("Synchronize Local → Remote") { state.sync(download: false) }
                Button("Synchronize Remote → Local") { state.sync(download: true) }
                Divider()
                Button("Find Files…") { state.showFind = true }
                    .keyboardShortcut("f", modifiers: .command)
                Button("Execute Command…") { state.beginExecCommand() }
                    .disabled(state.proto != .sftp || !state.isConnected)
                Button("Open Terminal") { state.openTerminal() }
                    .disabled(state.proto != .sftp || !state.isConnected)
                Divider()
                Button("Show Transfer Queue") {
                    TransferWindowController.shared.show(queue: state.transfers, state: state)
                }
                Button("Session Log") {
                    LogWindowController.shared.show(state: state)
                }
            }
            CommandMenu("Tabs") {
                Button("New Tab") { state.newTab() }
                    .keyboardShortcut("t", modifiers: [.command, .shift])
                Button("Close Tab") { state.closeTab(state.activeTab) }
                    .keyboardShortcut("w", modifiers: .command)
                Divider()
                Button("Next Tab") {
                    state.selectTab((state.activeTab + 1) % state.tabTitles.count)
                }
                .keyboardShortcut("]", modifiers: [.command, .shift])
                Button("Previous Tab") {
                    state.selectTab(
                        (state.activeTab - 1 + state.tabTitles.count) % state.tabTitles.count)
                }
                .keyboardShortcut("[", modifiers: [.command, .shift])
            }
            CommandMenu("Options") {
                Toggle("Show Hidden Files", isOn: $state.showHidden)
                    .keyboardShortcut(".", modifiers: [.command, .shift])
                Toggle("Mirror Mode (sync deletes extraneous)", isOn: $state.mirrorSync)
            }
            CommandMenu("Right") {
                paneMenu(.remote)
            }
            CommandGroup(replacing: .help) {
                Button("SCP Commander Help") {
                    HelpWindowController.shared.show()
                }
                .keyboardShortcut("/", modifiers: [.command, .shift])
            }
        }
    }

    /// Shared Left/Right pane menu: navigation commands for one pane.
    @ViewBuilder
    private func paneMenu(_ pane: PaneKind) -> some View {
        Button("Go Up") { pane == .local ? state.localUp() : state.remoteUp() }
        Button("Back") { state.goBack(pane) }
            .disabled(!state.canGoBack(pane))
        Button("Forward") { state.goForward(pane) }
            .disabled(!state.canGoForward(pane))
        Button("Home") { state.goHome(pane) }
        Divider()
        Button("Refresh") { pane == .local ? state.loadLocal() : state.refreshRemote() }
    }
}
