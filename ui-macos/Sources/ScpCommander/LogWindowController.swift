import AppKit
import SwiftUI

/// WinSCP-style session log: every status line, timestamped, in a floating
/// panel. Useful when a connection misbehaves and one status line isn't enough.
@MainActor
final class LogWindowController {
    static let shared = LogWindowController()
    private var panel: NSPanel?

    func show(state: AppState) {
        if let panel, panel.isVisible {
            panel.makeKeyAndOrderFront(nil)
            return
        }
        let panel = NSPanel(
            contentRect: .init(x: 0, y: 0, width: 560, height: 320),
            styleMask: [.titled, .closable, .resizable, .nonactivatingPanel],
            backing: .buffered,
            defer: false
        )
        panel.title = "Session Log"
        panel.isReleasedWhenClosed = false
        panel.contentView = NSHostingView(rootView: LogView(state: state))
        panel.center()
        panel.makeKeyAndOrderFront(nil)
        self.panel = panel
    }
}

private struct LogView: View {
    @ObservedObject var state: AppState

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Text("Session Log").font(.headline)
                Spacer()
                Button("Copy") {
                    NSPasteboard.general.clearContents()
                    NSPasteboard.general.setString(
                        state.logLines.joined(separator: "\n"), forType: .string)
                }
                .buttonStyle(.borderless)
                Button("Clear") { state.clearLog() }
                    .buttonStyle(.borderless)
            }
            .padding(.horizontal, 12).padding(.vertical, 8)
            Divider()
            ScrollViewReader { proxy in
                ScrollView {
                    VStack(alignment: .leading, spacing: 1) {
                        ForEach(Array(state.logLines.enumerated()), id: \.offset) { i, line in
                            Text(line)
                                .font(.system(size: 11, design: .monospaced))
                                .foregroundStyle(
                                    line.contains("Error") ? Color.red : Color.primary)
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .id(i)
                        }
                    }
                    .padding(8)
                }
                .onChange(of: state.logLines.count) { n in
                    if n > 0 { proxy.scrollTo(n - 1, anchor: .bottom) }
                }
            }
        }
        .frame(minWidth: 420, minHeight: 200)
    }
}
