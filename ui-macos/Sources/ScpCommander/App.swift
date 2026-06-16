import AppKit
import SwiftUI

/// Hosts the quit guard: AppKit asks the delegate before terminating, so a
/// running transfer can put up a confirmation instead of dying silently.
@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    weak var state: AppState?

    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        guard let state else { return .terminateNow }
        // Re-offer whatever didn't finish (failed rows too) on next launch.
        state.persistQueue()
        guard state.transfers.activeCount > 0 else { return .terminateNow }
        let n = state.transfers.activeCount
        let alert = NSAlert()
        alert.messageText = n == 1 ? "1 transfer is still running" : "\(n) transfers are still running"
        alert.informativeText = "Quitting now will cancel "
            + (n == 1 ? "it." : "them.")
        alert.alertStyle = .warning
        alert.addButton(withTitle: "Quit Anyway")
        alert.addButton(withTitle: "Keep Transferring")
        if alert.runModal() == .alertFirstButtonReturn {
            state.transfers.cancelAll()
            return .terminateNow
        }
        return .terminateCancel
    }
}

@main
struct ScpCommanderApp: App {
    @StateObject private var state = AppState()
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate

    var body: some Scene {
        WindowGroup("SCP Commander") {
            ContentView()
                .environmentObject(state)
                .onAppear { appDelegate.state = state }
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
                Button(
                    state.keepUpToDate == nil
                        ? "Keep Remote Directory Up To Date" : "Stop Keeping Up To Date"
                ) { state.toggleKeepUpToDate() }
                Divider()
                Button("Find Files…") { state.showFind = true }
                    .keyboardShortcut("f", modifiers: .command)
                Button("Execute Command…") { state.beginExecCommand() }
                    .disabled(state.proto != .sftp || !state.isConnected)
                Menu("Custom Commands") {
                    ForEach(state.customCommands) { cmd in
                        Button(cmd.name) { state.runCustomCommand(cmd) }
                            .disabled(state.proto != .sftp || !state.isConnected)
                    }
                    if !state.customCommands.isEmpty { Divider() }
                    Button("Manage Custom Commands…") { state.showCustomCommands = true }
                }
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
                Toggle("Synchronized Browsing", isOn: $state.syncBrowse)
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
        Settings {
            PreferencesView().environmentObject(state)
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
