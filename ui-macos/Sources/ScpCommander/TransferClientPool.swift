import Foundation

/// Pool of N persistent transfer connections dispatched round-robin.
/// Each slot has its own `CoreClient` and serial `DispatchQueue` so transfers
/// can run concurrently without contending on a single connection.
final class TransferClientPool {
    static let size = 3

    private struct Slot {
        let client: CoreClient
        let queue: DispatchQueue
    }

    private let slots: [Slot]
    private let lock = NSLock()
    private var nextIdx = 0

    init() {
        slots = (0..<Self.size).map { i in
            Slot(client: CoreClient(),
                 queue: DispatchQueue(label: "net.manto.ScpCommander.xfer\(i)"))
        }
    }

    /// Silently connect every slot in parallel. Errors are swallowed; the
    /// AutoReconnect wrapper in the Rust core retries on first use.
    func connectAll(proto: Proto, host: String, port: UInt16,
                    user: String, password: String,
                    bucket: String, region: String,
                    authMode: AuthMode, keyPath: String,
                    trustedFingerprint: String) {
        for slot in slots {
            slot.queue.async {
                try? slot.client.connect(
                    proto: proto, host: host, port: port,
                    user: user, password: password,
                    bucket: bucket, region: region,
                    hostKeyMode: trustedFingerprint.isEmpty ? .strict : .acceptFingerprint,
                    trustedFingerprint: trustedFingerprint,
                    authMode: authMode, keyPath: keyPath)
            }
        }
    }

    /// Dispatch `work` on the next slot in round-robin order.
    func submit(_ work: @escaping (CoreClient) -> Void) {
        lock.lock()
        let idx = nextIdx % slots.count
        nextIdx += 1
        lock.unlock()
        let slot = slots[idx]
        slot.queue.async { work(slot.client) }
    }
}
