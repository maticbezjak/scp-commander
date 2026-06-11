import AppKit
import SwiftUI

@MainActor
final class TransferWindowController {
    static let shared = TransferWindowController()
    private var panel: NSPanel?

    func show(queue: TransferQueue) {
        if let panel, panel.isVisible { return }
        let panel = NSPanel(
            contentRect: .init(x: 0, y: 0, width: 500, height: 320),
            styleMask: [.titled, .closable, .resizable, .nonactivatingPanel],
            backing: .buffered,
            defer: false
        )
        panel.title = "Transfer Queue"
        panel.isReleasedWhenClosed = false
        panel.contentView = NSHostingView(rootView: TransferQueueView(queue: queue))
        if let screen = NSScreen.main {
            let f = screen.visibleFrame
            panel.setFrameOrigin(.init(x: f.maxX - 520, y: f.minY + 20))
        } else {
            panel.center()
        }
        panel.makeKeyAndOrderFront(nil)
        self.panel = panel
    }
}

struct TransferQueueView: View {
    @ObservedObject var queue: TransferQueue

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Text("Transfers").font(.headline)
                Spacer()
                Button("Cancel All") { queue.cancelAll() }
                    .buttonStyle(.borderless)
                    .foregroundStyle(.red)
                    .disabled(queue.items.allSatisfy { $0.state != .active })
                Button("Clear Finished") { queue.clearFinished() }
                    .buttonStyle(.borderless)
                    .disabled(queue.items.allSatisfy { $0.state == .active })
            }
            .padding(.horizontal, 12).padding(.vertical, 8)
            Divider()
            if queue.items.isEmpty {
                Spacer()
                Text("No active transfers").foregroundStyle(.secondary)
                Spacer()
            } else {
                ScrollView {
                    VStack(spacing: 0) {
                        ForEach(queue.items) { TransferQueueRow(transfer: $0) }
                    }
                    .padding(.vertical, 4)
                }
            }
        }
        .frame(minWidth: 420, minHeight: 120)
    }
}

struct TransferQueueRow: View {
    @ObservedObject var transfer: Transfer

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: transfer.direction.symbol)
                .foregroundStyle(stateColor)
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
            progressView
            Spacer()
            Text(rowDetail)
                .font(.caption.monospacedDigit())
                .foregroundStyle(.secondary)
            if transfer.state == .active {
                Button {
                    if transfer.isPaused {
                        transfer.isPaused = false
                        transfer.pauseFlag.resume()
                    } else {
                        transfer.isPaused = true
                        transfer.pauseFlag.pause()
                    }
                } label: {
                    Image(systemName: transfer.isPaused ? "play.circle.fill" : "pause.circle.fill")
                        .foregroundStyle(.secondary)
                }
                .buttonStyle(.borderless)
                .help(transfer.isPaused ? "Resume" : "Pause")
                Button {
                    transfer.pauseFlag.resume()   // unblock worker before cancelling
                    transfer.cancelFlag.cancel()
                } label: {
                    Image(systemName: "xmark.circle.fill").foregroundStyle(.secondary)
                }
                .buttonStyle(.borderless)
                .help("Cancel")
            }
        }
        .padding(.horizontal, 12).padding(.vertical, 5)
    }

    private var stateColor: Color {
        switch transfer.state {
        case .active: return .accentColor
        case .done: return .green
        case .cancelled: return .orange
        case .failed: return .red
        }
    }

    @ViewBuilder
    private var progressView: some View {
        switch transfer.state {
        case .active:
            if let f = transfer.fraction {
                ProgressView(value: f)
                    .frame(width: 160)
                    .opacity(transfer.isPaused ? 0.4 : 1.0)
            } else if !transfer.isPaused {
                ProgressView().scaleEffect(0.5).frame(width: 40)
            } else {
                Image(systemName: "pause.circle").foregroundStyle(.secondary)
            }
        case .done:
            Image(systemName: "checkmark.circle.fill").foregroundStyle(.green)
        case .cancelled:
            Image(systemName: "slash.circle").foregroundStyle(.orange)
        case .failed:
            Image(systemName: "xmark.octagon.fill").foregroundStyle(.red)
        }
    }

    private var rowDetail: String {
        switch transfer.state {
        case .failed(let msg): return msg
        case .cancelled: return "cancelled"
        case .done:
            let files = transfer.filesDone > 0 ? "\(transfer.filesDone) files · " : ""
            return files + humanSize(max(transfer.total, transfer.transferred))
        case .active:
            if transfer.isPaused { return "paused" }
            var parts: [String] = []
            if transfer.total > 0 {
                parts.append("\(humanSize(transfer.transferred)) / \(humanSize(transfer.total))")
            } else {
                parts.append(humanSize(transfer.transferred))
            }
            if transfer.speed > 1 {
                parts.append("\(humanSize(UInt64(transfer.speed)))/s")
            }
            if let eta = transfer.eta { parts.append(eta) }
            return parts.joined(separator: " · ")
        }
    }
}
