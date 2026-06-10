import Foundation

/// A saved connection — the full session, WinSCP-style: protocol, endpoint,
/// auth method, key file, and S3 bucket/region. Passwords live in the
/// Keychain (only when the user opts in at save time), never in this file.
///
/// Site names may contain "/" to group sites into folders, exactly like
/// WinSCP: "Work/web1" shows as "web1" inside a "Work" folder.
struct Site: Codable, Identifiable, Hashable {
    var id = UUID()
    var name: String
    var proto: Proto
    var host: String
    var port: String
    var user: String
    var authMode: AuthMode
    var keyPath: String
    var bucket: String
    var region: String

    init(
        name: String, proto: Proto, host: String, port: String, user: String,
        authMode: AuthMode = .password, keyPath: String = "",
        bucket: String = "", region: String = ""
    ) {
        self.name = name
        self.proto = proto
        self.host = host
        self.port = port
        self.user = user
        self.authMode = authMode
        self.keyPath = keyPath
        self.bucket = bucket
        self.region = region
    }

    /// Decode tolerantly so sites.json files from older versions still load.
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decodeIfPresent(UUID.self, forKey: .id) ?? UUID()
        name = try c.decode(String.self, forKey: .name)
        proto = try c.decodeIfPresent(Proto.self, forKey: .proto) ?? .sftp
        host = try c.decodeIfPresent(String.self, forKey: .host) ?? ""
        port = try c.decodeIfPresent(String.self, forKey: .port) ?? "22"
        user = try c.decodeIfPresent(String.self, forKey: .user) ?? ""
        authMode = try c.decodeIfPresent(AuthMode.self, forKey: .authMode) ?? .password
        keyPath = try c.decodeIfPresent(String.self, forKey: .keyPath) ?? ""
        bucket = try c.decodeIfPresent(String.self, forKey: .bucket) ?? ""
        region = try c.decodeIfPresent(String.self, forKey: .region) ?? ""
    }

    /// WinSCP-style folder: the part before the first "/", if any.
    var folder: String? {
        guard let idx = name.firstIndex(of: "/") else { return nil }
        let f = String(name[..<idx])
        return f.isEmpty ? nil : f
    }

    /// Name shown in the list (folder prefix stripped).
    var displayName: String {
        guard let idx = name.firstIndex(of: "/") else { return name }
        return String(name[name.index(after: idx)...])
    }

    var keychainAccount: String {
        Keychain.account(proto: proto, user: user, host: host, port: port)
    }
}

// MARK: - Interchange format (shared with the GTK app)

/// Versioned, human-readable export format. Both the macOS and Ubuntu apps
/// read and write this, so sites can move between machines and platforms.
/// Passwords are intentionally not part of it — they stay in the Keychain.
struct SiteExportFile: Codable {
    var scp_commander_sites = 1
    var sites: [SiteExport]
}

struct SiteExport: Codable {
    var name: String
    var `protocol`: String  // sftp | ftp | ftps | s3
    var host: String
    var port: String
    var user: String
    var auth: String  // password | key | agent
    var key_path: String
    var bucket: String
    var region: String

    init(from site: Site) {
        name = site.name
        `protocol` = ["sftp", "ftp", "ftps", "s3"][Int(site.proto.rawValue) % 4]
        host = site.host
        port = site.port
        user = site.user
        auth = ["password", "key", "agent"][Int(site.authMode.rawValue) % 3]
        key_path = site.keyPath
        bucket = site.bucket
        region = site.region
    }

    var asSite: Site {
        let proto: Proto =
            switch `protocol` {
            case "ftp": .ftp
            case "ftps": .ftps
            case "s3": .s3
            default: .sftp
            }
        let mode: AuthMode =
            switch auth {
            case "key": .keyFile
            case "agent": .agent
            default: .password
            }
        return Site(
            name: name, proto: proto, host: host, port: port, user: user,
            authMode: mode, keyPath: key_path, bucket: bucket, region: region)
    }
}

/// Saved sites, persisted as JSON under Application Support, kept sorted by
/// name so folder groups stay together.
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

    /// Folder names (in display order), with nil for ungrouped sites first.
    var folders: [String?] {
        var seen = [String?]()
        for site in sites {
            if !seen.contains(site.folder) { seen.append(site.folder) }
        }
        // Ungrouped first, then folders alphabetically.
        return seen.sorted { a, b in
            switch (a, b) {
            case (nil, _): return true
            case (_, nil): return false
            case (let x?, let y?): return x.localizedCaseInsensitiveCompare(y) == .orderedAscending
            }
        }
    }

    func sites(in folder: String?) -> [Site] {
        sites.filter { $0.folder == folder }
    }

    func add(_ site: Site) {
        // Replace a same-named entry rather than duplicating.
        if let idx = sites.firstIndex(where: { $0.name == site.name }) {
            sites[idx] = site
        } else {
            sites.append(site)
        }
        sortAndSave()
    }

    func rename(_ site: Site, to newName: String) {
        guard !newName.isEmpty, let idx = sites.firstIndex(where: { $0.id == site.id }) else {
            return
        }
        sites[idx].name = newName
        sortAndSave()
    }

    func remove(_ site: Site) {
        sites.removeAll { $0.id == site.id }
        sortAndSave()
    }

    /// Serialize all sites to the cross-platform interchange format.
    func exportData() throws -> Data {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        return try encoder.encode(SiteExportFile(sites: sites.map(SiteExport.init)))
    }

    /// Merge sites from interchange data (same-named sites are replaced).
    /// Returns the number of sites in the file.
    func importData(_ data: Data) throws -> Int {
        let file = try JSONDecoder().decode(SiteExportFile.self, from: data)
        for exported in file.sites {
            add(exported.asSite)
        }
        return file.sites.count
    }

    private func sortAndSave() {
        sort()
        save()
    }

    private func sort() {
        sites.sort { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
    }

    private func load() {
        guard let data = try? Data(contentsOf: fileURL) else { return }
        sites = (try? JSONDecoder().decode([Site].self, from: data)) ?? []
        sort()
    }

    private func save() {
        if let data = try? JSONEncoder().encode(sites) {
            try? data.write(to: fileURL, options: .atomic)
        }
    }
}
