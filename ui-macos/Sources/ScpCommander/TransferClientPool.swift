import Foundation

/// Pool of N persistent transfer connections dispatched round-robin.
/// Each slot has its own `CoreClient` and serial `DispatchQueue` so transfers
/// can run concurrently without contending on a single connection.
final class TransferClientPool {
    /// Parallel connections per session, configurable in Preferences
    /// (clamped 1…8; read when a pool is created, i.e. at connect time).
    static var size: Int {
        let v = UserDefaults.standard.integer(forKey: "transferPoolSize")
        return v == 0 ? 3 : min(8, max(1, v))
    }

    private struct Slot {
        let client: CoreClient
        let queue: DispatchQueue
    }

    /// Connection parameters kept for lazy reconnects of failed slots.
    private struct ConnectParams {
        let proto: Proto
        let host: String
        let port: UInt16
        let user: String
        let password: String
        let bucket: String
        let region: String
        let authMode: AuthMode
        let keyPath: String
        let trustedFingerprint: String
        let jumpHost: String
        let jumpPort: UInt16
        let jumpUser: String
        let jumpPassword: String
        let jumpAuthMode: AuthMode
        let jumpKeyPath: String
    }

    private let slots: [Slot]
    private let lock = NSLock()
    private var nextIdx = 0
    private var params: ConnectParams?

    init() {
        slots = (0..<Self.size).map { i in
            Slot(client: CoreClient(),
                 queue: DispatchQueue(label: "net.manto.ScpCommander.xfer\(i)"))
        }
    }

    /// Silently connect every slot in parallel. Errors are swallowed here —
    /// a slot whose connect failed retries lazily on its next `submit`.
    func connectAll(proto: Proto, host: String, port: UInt16,
                    user: String, password: String,
                    bucket: String, region: String,
                    authMode: AuthMode, keyPath: String,
                    trustedFingerprint: String,
                    jumpHost: String = "", jumpPort: UInt16 = 22,
                    jumpUser: String = "", jumpPassword: String = "",
                    jumpAuthMode: AuthMode = .password, jumpKeyPath: String = "") {
        let p = ConnectParams(
            proto: proto, host: host, port: port, user: user, password: password,
            bucket: bucket, region: region, authMode: authMode, keyPath: keyPath,
            trustedFingerprint: trustedFingerprint,
            jumpHost: jumpHost, jumpPort: jumpPort, jumpUser: jumpUser,
            jumpPassword: jumpPassword, jumpAuthMode: jumpAuthMode, jumpKeyPath: jumpKeyPath)
        lock.lock()
        params = p
        lock.unlock()
        for slot in slots {
            slot.queue.async { Self.connect(slot.client, p) }
        }
    }

    private static func connect(_ client: CoreClient, _ p: ConnectParams) {
        try? client.connect(
            proto: p.proto, host: p.host, port: p.port,
            user: p.user, password: p.password,
            bucket: p.bucket, region: p.region,
            hostKeyMode: p.trustedFingerprint.isEmpty ? .strict : .acceptFingerprint,
            trustedFingerprint: p.trustedFingerprint,
            authMode: p.authMode, keyPath: p.keyPath,
            jumpHost: p.jumpHost, jumpPort: p.jumpPort, jumpUser: p.jumpUser,
            jumpPassword: p.jumpPassword, jumpAuthMode: p.jumpAuthMode, jumpKeyPath: p.jumpKeyPath)
    }

    /// Dispatch `work` on the next slot in round-robin order, reviving the
    /// slot's connection first if its silent connect never succeeded.
    func submit(_ work: @escaping (CoreClient) -> Void) {
        lock.lock()
        let idx = nextIdx % slots.count
        nextIdx += 1
        let p = params
        lock.unlock()
        let slot = slots[idx]
        slot.queue.async {
            if !slot.client.isConnected, let p {
                Self.connect(slot.client, p)
            }
            work(slot.client)
        }
    }
}
