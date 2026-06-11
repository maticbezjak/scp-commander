import SwiftUI

struct HelpView: View {
    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 0) {
                helpSection("Interface overview") {
                    interfaceDiagram
                }

                helpSection("Icons reference") {
                    iconGroup("Main toolbar") {
                        iconRow("network",                    "New Session",       "Open the Login dialog to connect to a server or switch sites.")
                        iconRow("eye.slash",                  "Show hidden",       "Toggle files whose names start with a dot (hidden files).")
                        iconRow("arrow.triangle.2.circlepath","Synchronize",       "Sync local ↔ remote. Choose direction from the dropdown; tick Mirror to also delete extras.")
                        iconRow("terminal.fill",              "Execute command",   "Run a shell command on the remote server (SFTP only). Output shown in a dialog.")
                        iconRow("magnifyingglass",            "Find files",        "Search the remote directory recursively by name mask (e.g. *.log).")
                        iconRow("terminal",                   "Open terminal",     "Open a new SSH session to the current host in your system Terminal app.")
                        iconRow("questionmark.circle",        "Help",              "Open this help window.")
                    }
                    iconGroup("Pane header — both panes") {
                        iconRow("arrow.up",                   "Parent directory",  "Navigate up one level (same as the .. row or Backspace).")
                        iconRow("arrow.clockwise",            "Refresh",           "Reload the current directory listing.")
                        iconRow("folder.badge.plus",          "New folder",        "Create a new folder inside the current directory.")
                        iconRow("trash",                      "Delete",            "Delete the selected file(s) or folder(s). Folders are removed recursively.")
                    }
                    iconGroup("Pane header — local pane") {
                        iconRow("arrow.up.circle",            "Upload (F5)",       "Copy selected local items to the current remote directory.")
                    }
                    iconGroup("Pane header — remote pane") {
                        iconRow("arrow.down.circle",          "Download (F5)",     "Copy selected remote items to the current local directory.")
                        iconRow("pencil",                     "Edit",              "Download the selected file, open it in your editor, and auto-upload on every save.")
                    }
                    iconGroup("File list — row icons") {
                        iconRow("folder.fill",                "Directory",         "A folder — double-click to navigate into it.")
                        iconRow("doc",                        "File",              "A regular file — double-click to transfer it.")
                        iconRow("arrow.right.circle",         "Symlink",           "A symbolic link.")
                        iconRow("arrow.up.left",              ".. (parent)",       "The top row in every listing — double-click to go up one level.")
                    }
                }

                helpSection("Connecting") {
                    helpStep("1", "Open the Login dialog — it appears on launch or via the Login button in the toolbar.")
                    helpStep("2", "Choose a protocol (SFTP · FTP · FTPS · S3), fill in host, port, and credentials.")
                    helpStep("3", "Tick Remember password to store it securely in the Keychain. Next time you type the same host + user it fills in automatically.")
                    helpStep("4", "Click Login (or press Return). If the server's host key is new, review the fingerprint and click Trust & Connect.")
                }

                helpSection("Saving sites") {
                    helpPara("Click Save site… in the Login dialog to bookmark the current credentials. Use Folder/Name format to group them (e.g. Work/Production). Double-click a saved site to connect instantly. Right-click a site to rename or delete it.")
                }

                helpSection("Browsing") {
                    helpRow("Left pane", "Your local filesystem")
                    helpRow("Right pane", "The remote server")
                    helpRow("Double-click folder", "Navigate into it")
                    helpRow(".. row / Backspace", "Go up one level")
                    helpRow("Path bar", "Type a path and press Return to jump directly")
                    helpRow("Column header click", "Sort by Name, Size, Type, Changed, or Rights")
                    helpRow("Eye icon", "Toggle hidden files (names starting with .)")
                    helpRow("Filter box", "Type to narrow the visible listing by name")
                }

                helpSection("Transferring files") {
                    helpRow("F5 or ↑/↓ button", "Copy selected items to the other pane")
                    helpRow("F6", "Move selected items (copy then delete source)")
                    helpRow("Drag and drop", "Drag files between the two panes")
                    helpPara("Folders transfer recursively. If the destination already has a file with the same name you get an Overwrite / Skip / Cancel prompt. Transfers run in the background — keep browsing while files copy.")
                }

                helpSection("Multi-select") {
                    helpRow("Click", "Select one item")
                    helpRow("Shift-click", "Extend the selection to a range")
                    helpRow("Cmd-click", "Toggle individual items in/out of the selection")
                    helpPara("All selected items transfer together with F5/F6 or drag.")
                }

                helpSection("Keyboard shortcuts") {
                    shortcutRow("F5", "Copy (transfer) selected items")
                    shortcutRow("F6", "Move selected items")
                    shortcutRow("F2", "Rename selected item")
                    shortcutRow("Delete", "Delete selected item(s)")
                    shortcutRow("Backspace", "Navigate to parent directory")
                    shortcutRow("Tab", "Switch focus between left and right pane")
                    shortcutRow("Return", "Open folder / transfer file")
                }

                helpSection("Directory sync") {
                    helpPara("Click ↑ sync or ↓ sync in the toolbar to synchronise a pair of local/remote directories. A preview checklist shows which files will be copied or deleted — review and confirm. Tick Mirror to also delete destination items that have no source counterpart.")
                }

                helpSection("Finding files") {
                    helpPara("Click the 🔍 button to search the current remote directory recursively by name mask (e.g. *.log). Double-click any result to navigate to its directory.")
                }

                helpSection("Remote editing") {
                    helpPara("Right-click a remote file → Edit. The file downloads to a temp location and opens in your default editor. Every time you save, it uploads automatically.")
                }

                helpSection("Transfer queue") {
                    helpPara("The panel at the bottom shows all active and completed transfers with live progress bars, speed, and ETA. Each transfer has its own × cancel button. Cancel All stops every running transfer at once.")
                }
            }
            .padding(20)
        }
        .frame(width: 560, height: 520)
    }

    private var interfaceDiagram: some View {
        VStack(alignment: .leading, spacing: 10) {
            // ── Annotated mock of the window layout ──────────────────────────
            VStack(spacing: 0) {
                // Toolbar row
                uiRow(color: .blue.opacity(0.12)) {
                    badge("1", .blue)
                    Text("Toolbar").font(.caption.bold())
                    Spacer()
                    Text("New Session · eye · host · Synchronize · terminal · search · exclude · shortcuts · ?")
                        .font(.caption2).foregroundStyle(.secondary)
                }
                // Tab bar
                uiRow(color: .purple.opacity(0.10)) {
                    badge("2", .purple)
                    Text("Session tabs").font(.caption.bold())
                    Spacer()
                    Text("demo@127.0.0.1 ×   +")
                        .font(.caption2).foregroundStyle(.secondary)
                }
                // Pane headers
                HStack(spacing: 0) {
                    uiRow(color: .green.opacity(0.12)) {
                        badge("3", .green)
                        Text("Left pane header").font(.caption.bold())
                        Spacer()
                        Text("Local  ↑ ↻ ↑ 📁 🗑  filter")
                            .font(.caption2).foregroundStyle(.secondary)
                    }
                    Divider()
                    uiRow(color: .green.opacity(0.12)) {
                        badge("3", .green)
                        Text("Right pane header").font(.caption.bold())
                        Spacer()
                        Text("Remote  ↑ ↻ ↓ ✏ 📁 🗑  filter")
                            .font(.caption2).foregroundStyle(.secondary)
                    }
                }
                // Path bars
                HStack(spacing: 0) {
                    uiRow(color: .orange.opacity(0.12)) {
                        badge("4", .orange)
                        Image(systemName: "folder").font(.caption)
                        Text("/Users/matic.bezjak").font(.caption.monospacedDigit())
                        Spacer()
                    }
                    Divider()
                    uiRow(color: .orange.opacity(0.12)) {
                        badge("4", .orange)
                        Image(systemName: "folder").font(.caption)
                        Text("/").font(.caption.monospacedDigit())
                        Spacer()
                    }
                }
                // Column headers
                HStack(spacing: 0) {
                    uiRow(color: .gray.opacity(0.10)) {
                        badge("5", .gray)
                        Text("Name  ·  Size  ·  Type  ·  Changed").font(.caption2)
                        Spacer()
                    }
                    Divider()
                    uiRow(color: .gray.opacity(0.10)) {
                        badge("5", .gray)
                        Text("Name  ·  Size  ·  Type  ·  Changed  ·  Owner  ·  Rights").font(.caption2)
                        Spacer()
                    }
                }
            }
            .overlay(RoundedRectangle(cornerRadius: 6).stroke(Color.secondary.opacity(0.2)))
            .clipShape(RoundedRectangle(cornerRadius: 6))

            // ── Legend ───────────────────────────────────────────────────────
            VStack(alignment: .leading, spacing: 4) {
                legendRow("1", .blue,   "Toolbar",        "Session controls, sync, find, terminal, excludes, and help (?)")
                legendRow("2", .purple, "Session tabs",   "Each tab is an independent connection. Click + for a new session, × to close.")
                legendRow("3", .green,  "Pane header",    "↑ parent · ↻ refresh · ↑/↓ transfer · ✏ edit · 📁 new folder · 🗑 delete · filter box")
                legendRow("4", .orange, "Address bar",    "Shows the current path. Click to edit and press Return to jump to any directory.")
                legendRow("5", .gray,   "Column headers", "Click a header to sort. Remote pane adds Owner and Rights columns (SFTP only).")
            }
        }
    }

    private func uiRow(color: Color, @ViewBuilder content: () -> some View) -> some View {
        HStack(spacing: 6) { content() }
            .padding(.horizontal, 8)
            .padding(.vertical, 5)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(color)
    }

    private func badge(_ label: String, _ color: Color) -> some View {
        Text(label)
            .font(.system(size: 9, weight: .bold))
            .foregroundStyle(.white)
            .frame(width: 14, height: 14)
            .background(color)
            .clipShape(Circle())
    }

    private func legendRow(_ num: String, _ color: Color, _ name: String, _ desc: String) -> some View {
        HStack(alignment: .top, spacing: 8) {
            badge(num, color)
            Text(name).font(.callout.bold()).frame(width: 110, alignment: .leading)
            Text(desc).font(.callout).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            Spacer()
        }
    }

    private func iconGroup<Content: View>(_ label: String, @ViewBuilder content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(label)
                .font(.caption.bold())
                .foregroundStyle(.secondary)
                .padding(.top, 6)
            content()
        }
    }

    private func iconRow(_ symbol: String, _ name: String, _ desc: String) -> some View {
        HStack(alignment: .center, spacing: 10) {
            Image(systemName: symbol)
                .font(.system(size: 14))
                .foregroundStyle(Color.accentColor)
                .frame(width: 22, alignment: .center)
            Text(name)
                .font(.callout.bold())
                .frame(width: 130, alignment: .leading)
            Text(desc)
                .font(.callout)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            Spacer()
        }
        .padding(.vertical, 2)
    }

    private func helpSection<Content: View>(_ title: String, @ViewBuilder content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(title)
                .font(.headline)
                .padding(.top, 16)
            content()
            Divider().padding(.top, 4)
        }
    }

    private func helpStep(_ number: String, _ text: String) -> some View {
        HStack(alignment: .top, spacing: 8) {
            Text(number)
                .font(.caption.bold())
                .foregroundStyle(.white)
                .frame(width: 18, height: 18)
                .background(Color.accentColor)
                .clipShape(Circle())
            Text(text)
                .font(.callout)
                .fixedSize(horizontal: false, vertical: true)
            Spacer()
        }
    }

    private func helpPara(_ text: String) -> some View {
        Text(text)
            .font(.callout)
            .fixedSize(horizontal: false, vertical: true)
            .padding(.leading, 2)
    }

    private func helpRow(_ label: String, _ detail: String) -> some View {
        HStack(alignment: .top, spacing: 0) {
            Text(label)
                .font(.callout.bold())
                .frame(width: 160, alignment: .leading)
            Text(detail)
                .font(.callout)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            Spacer()
        }
    }

    private func shortcutRow(_ key: String, _ action: String) -> some View {
        HStack(spacing: 8) {
            Text(key)
                .font(.system(.caption, design: .monospaced).bold())
                .padding(.horizontal, 6)
                .padding(.vertical, 2)
                .background(Color.secondary.opacity(0.15))
                .clipShape(RoundedRectangle(cornerRadius: 4))
                .frame(width: 80, alignment: .center)
            Text(action)
                .font(.callout)
                .foregroundStyle(.secondary)
            Spacer()
        }
    }
}
