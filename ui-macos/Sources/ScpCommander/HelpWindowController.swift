import AppKit
import SwiftUI

final class HelpWindowController {
    static let shared = HelpWindowController()

    private var panel: NSPanel?

    func show() {
        if let panel, panel.isVisible {
            panel.makeKeyAndOrderFront(nil)
            return
        }

        let panel = NSPanel(
            contentRect: .init(x: 0, y: 0, width: 560, height: 560),
            styleMask: [.titled, .closable, .resizable, .nonactivatingPanel],
            backing: .buffered,
            defer: false
        )
        panel.title = "SCP Commander — Help"
        panel.isReleasedWhenClosed = false
        panel.contentView = NSHostingView(rootView: HelpView())
        panel.center()
        panel.makeKeyAndOrderFront(nil)
        self.panel = panel
    }
}
