import Foundation

enum TransferDirection {
    case upload
    case download

    var symbol: String {
        switch self {
        case .upload: return "arrow.up.circle"
        case .download: return "arrow.down.circle"
        }
    }
}

enum TransferState: Equatable {
    case active
    case done
    case cancelled
    case failed(String)
}

/// One transfer (or multi-file operation) in the queue. Observed per-row so
/// progress updates live. `cancelFlag` is checked by the worker-side progress
/// callback; the Cancel button flips it.
@MainActor
final class Transfer: ObservableObject, Identifiable {
    let id = UUID()
    let name: String
    let direction: TransferDirection
    let cancelFlag = CancelFlag()

    @Published var transferred: UInt64 = 0
    @Published var total: UInt64 = 0
    @Published var state: TransferState = .active
    /// For multi-file operations: the file currently being copied.
    @Published var currentFile: String?
    @Published var filesDone: Int = 0

    init(name: String, direction: TransferDirection) {
        self.name = name
        self.direction = direction
    }

    /// 0…1, or nil when the total size is unknown (shows as indeterminate).
    var fraction: Double? {
        guard total > 0 else { return nil }
        return min(1.0, Double(transferred) / Double(total))
    }
}

/// The list of transfers shown in the bottom panel.
@MainActor
final class TransferQueue: ObservableObject {
    @Published private(set) var items: [Transfer] = []

    func add(_ transfer: Transfer) {
        items.insert(transfer, at: 0)
    }

    func clearFinished() {
        items.removeAll { $0.state != .active }
    }
}
