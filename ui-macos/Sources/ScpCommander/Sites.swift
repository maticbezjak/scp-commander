import Foundation

/// A saved connection. Passwords are intentionally NOT persisted — the user
/// enters the password at connect time (a real app would use the Keychain).
struct Site: Codable, Identifiable, Hashable {
    var id = UUID()
    var name: String
    var proto: Proto
    var host: String
    var port: String
    var user: String
}

/// Saved sites, persisted as JSON under Application Support.
@MainActor
final class SitesStore: ObservableObject {
    @Published private(set) var sites: [Site] = []

    private let fileURL: URL

    init() {
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)
            .first ?? FileManager.default.temporaryDirectory
        let dir = base.appendingPathComponent("ScpCommander", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        fileURL = dir.appendingPathComponent("sites.json")
        load()
    }

    func add(_ site: Site) {
        // Replace a same-named entry rather than duplicating.
        if let idx = sites.firstIndex(where: { $0.name == site.name }) {
            sites[idx] = site
        } else {
            sites.append(site)
        }
        save()
    }

    func remove(_ site: Site) {
        sites.removeAll { $0.id == site.id }
        save()
    }

    private func load() {
        guard let data = try? Data(contentsOf: fileURL) else { return }
        sites = (try? JSONDecoder().decode([Site].self, from: data)) ?? []
    }

    private func save() {
        if let data = try? JSONEncoder().encode(sites) {
            try? data.write(to: fileURL, options: .atomic)
        }
    }
}
