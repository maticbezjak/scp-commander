import AppKit
import SwiftUI

@MainActor
final class TransferWindowController {
    static let shared = TransferWindowController()
    private var panel: NSPanel?

    func show(queue: TransferQueue, state: AppState? = nil) {
        if let panel, panel.isVisible { return }
        let panel = NSPanel(
            contentRect: .init(x: 0, y: 0, width: 460, height: 340),
            styleMask: [.titled, .closable, .resizable, .nonactivatingPanel],
            backing: .buffered,
            defer: false
        )
        panel.title = "Transfers"
        panel.isReleasedWhenClosed = false
        panel.contentView = NSHostingView(
            rootView: TransferQueueView(queue: queue, state: state))
        if let screen = NSScreen.main {
            let f = screen.visibleFrame
            panel.setFrameOrigin(.init(x: f.maxX - 480, y: f.minY + 20))
        } else {
            panel.center()
        }
        panel.makeKeyAndOrderFront(nil)
        self.panel = panel
    }

    /// WinSCP titles the dialog with live progress ("17% Uploading").
    func setTitle(_ title: String) {
        panel?.title = title
    }
}

struct TransferQueueView: View {
    @ObservedObject var queue: TransferQueue
    var state: AppState?

    static func bytes(_ n: UInt64) -> String {
        ByteCountFormatter.string(fromByteCount: Int64(n), countStyle: .file)
    }

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
            if let agg = queue.aggregate {
                let pct = agg.total > 0
                    ? Int((Double(agg.transferred) / Double(agg.total) * 100).rounded()) : 0
                VStack(spacing: 3) {
                    HStack {
                        Text("\(queue.activeCount) active")
                        Spacer()
                        Text("\(Self.bytes(agg.transferred)) / \(Self.bytes(agg.total)) · \(pct)%")
                    }
                    .font(.caption).foregroundStyle(.secondary)
                    ProgressView(
                        value: Double(agg.transferred),
                        total: Double(max(agg.total, 1)))
                }
                .padding(.horizontal, 12).padding(.bottom, 6)
            }
            Divider()
            if queue.items.isEmpty {
                Spacer()
                Text("No active transfers").foregroundStyle(.secondary)
                Spacer()
            } else {
                ScrollView {
                    VStack(spacing: 6) {
                        ForEach(queue.items) { transfer in
                            if transfer.state == .active {
                                ActiveTransferCard(transfer: transfer)
                            } else {
                                FinishedTransferRow(transfer: transfer)
                            }
                        }
                    }
                    .padding(8)
                }
            }
            if let state {
                Divider()
                SpeedLimitFooter(state: state)
            }
        }
        .frame(minWidth: 440, minHeight: 160)
        .onReceive(Timer.publish(every: 1, on: .main, in: .common).autoconnect()) { _ in
            if let first = queue.items.first(where: { $0.state == .active }) {
                let op = first.direction == .upload ? "Uploading" : "Downloading"
                if let f = first.fraction {
                    TransferWindowController.shared.setTitle("\(Int((f * 100).rounded()))% \(op)")
                } else {
                    TransferWindowController.shared.setTitle(op)
                }
            } else {
                TransferWindowController.shared.setTitle("Transfers")
            }
        }
    }
}

/// WinSCP-style progress card: % header, File/Target, bar, time/bytes grid.
private struct ActiveTransferCard: View {
    @ObservedObject var transfer: Transfer

    private var percent: Int {
        Int(((transfer.fraction ?? 0) * 100).rounded())
    }

    private var operation: String {
        transfer.direction == .upload ? "Uploading" : "Downloading"
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            // ── "17% Uploading" + pause/cancel ─────────────────────────────
            HStack {
                Image(systemName: transfer.direction.symbol)
                    .foregroundStyle(Color.accentColor)
                Text(transfer.fraction != nil ? "\(percent)% \(operation)" : operation)
                    .font(.headline)
                if transfer.isPaused {
                    Text("— paused").foregroundStyle(.secondary)
                }
                Spacer()
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
                }
                .buttonStyle(.borderless)
                .help(transfer.isPaused ? "Resume" : "Pause")
                Button {
                    transfer.pauseFlag.resume()   // unblock worker before cancelling
                    transfer.cancelFlag.cancel()
                } label: {
                    Image(systemName: "xmark.circle.fill").foregroundStyle(.red)
                }
                .buttonStyle(.borderless)
                .help("Cancel")
            }

            // ── File: / Target: ────────────────────────────────────────────
            Grid(alignment: .leading, horizontalSpacing: 8, verticalSpacing: 2) {
                GridRow {
                    Text("File:").foregroundStyle(.secondary)
                    Text(transfer.currentFile ?? transfer.source)
                        .lineLimit(1)
                        .truncationMode(.head)
                }
                GridRow {
                    Text("Target:").foregroundStyle(.secondary)
                    Text(transfer.target)
                        .lineLimit(1)
                        .truncationMode(.head)
                }
            }
            .font(.caption)

            ProgressView(value: transfer.fraction ?? 0)
                .opacity(transfer.isPaused ? 0.4 : 1.0)

            // ── Time left / elapsed · Bytes / Speed ────────────────────────
            TimelineView(.periodic(from: .now, by: 1)) { _ in
                Grid(alignment: .leading, horizontalSpacing: 8, verticalSpacing: 2) {
                    GridRow {
                        Text("Time left:").foregroundStyle(.secondary)
                        Text(transfer.eta ?? "—").monospacedDigit()
                        Spacer().gridCellUnsizedAxes([.horizontal, .vertical])
                        Text("Time elapsed:").foregroundStyle(.secondary)
                        Text(transfer.elapsed).monospacedDigit()
                    }
                    GridRow {
                        Text("Bytes transferred:").foregroundStyle(.secondary)
                        Text(humanSize(transfer.transferred)).monospacedDigit()
                        Spacer().gridCellUnsizedAxes([.horizontal, .vertical])
                        Text("Speed:").foregroundStyle(.secondary)
                        Text(transfer.speed > 1 ? "\(humanSize(UInt64(transfer.speed)))/s" : "—")
                            .monospacedDigit()
                    }
                }
                .font(.caption)
            }
        }
        .padding(10)
        .background(
            RoundedRectangle(cornerRadius: 8)
                .fill(Color.primary.opacity(0.05))
        )
    }
}

/// Compact one-line row for finished/cancelled/failed transfers.
private struct FinishedTransferRow: View {
    @ObservedObject var transfer: Transfer

    var body: some View {
        HStack(spacing: 8) {
            switch transfer.state {
            case .done:
                Image(systemName: "checkmark.circle.fill").foregroundStyle(.green)
            case .cancelled:
                Image(systemName: "slash.circle").foregroundStyle(.orange)
            case .failed:
                Image(systemName: "xmark.octagon.fill").foregroundStyle(.red)
            case .active:
                EmptyView()
            }
            Text(transfer.name).lineLimit(1)
            Spacer()
            Text(detail)
                .font(.caption.monospacedDigit())
                .foregroundStyle(.secondary)
                .lineLimit(1)
            if transfer.state != .done, let retry = transfer.retry {
                Button {
                    retry()
                } label: {
                    Image(systemName: "arrow.clockwise.circle.fill")
                        .foregroundStyle(Color.accentColor)
                }
                .buttonStyle(.borderless)
                .help("Retry this transfer")
            }
        }
        .padding(.horizontal, 10).padding(.vertical, 4)
    }

    private var detail: String {
        switch transfer.state {
        case .failed(let msg): return msg
        case .cancelled: return "cancelled"
        case .done:
            let files = transfer.filesDone > 0 ? "\(transfer.filesDone) files · " : ""
            return files + humanSize(max(transfer.total, transfer.transferred))
        case .active: return ""
        }
    }
}

/// WinSCP's bottom-row speed limit dropdown ("Unlimited").
private struct SpeedLimitFooter: View {
    @ObservedObject var state: AppState

    var body: some View {
        HStack {
            Image(systemName: "speedometer")
                .foregroundStyle(.secondary)
            Picker("Speed limit", selection: $state.speedLimitKbs) {
                Text("Unlimited").tag(0)
                Text("100 KiB/s").tag(100)
                Text("500 KiB/s").tag(500)
                Text("1 MiB/s").tag(1024)
                Text("5 MiB/s").tag(5120)
            }
            .labelsHidden()
            .fixedSize()
            Spacer()
        }
        .padding(.horizontal, 12).padding(.vertical, 6)
    }
}
