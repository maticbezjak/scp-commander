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
    /// Initial directories applied when the site is loaded (WinSCP's
    /// "Remote directory" advanced setting). Empty = defaults.
    var remoteDir: String
    var localDir: String
    /// Optional SFTP bastion (Optional so older saved sites still decode).
    var jumpHost: String?
    var jumpPort: String?
    var jumpUser: String?
    var jumpAuthMode: AuthMode?
    var jumpKeyPath: String?

    init(
        name: String, proto: Proto, host: String, port: String, user: String,
        authMode: AuthMode = .password, keyPath: String = "",
        bucket: String = "", region: String = "",
        remoteDir: String = "", localDir: String = "",
        jumpHost: String? = nil, jumpPort: String? = nil, jumpUser: String? = nil,
        jumpAuthMode: AuthMode? = nil, jumpKeyPath: String? = nil
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
        self.remoteDir = remoteDir
        self.localDir = localDir
        self.jumpHost = jumpHost
        self.jumpPort = jumpPort
        self.jumpUser = jumpUser
        self.jumpAuthMode = jumpAuthMode
        self.jumpKeyPath = jumpKeyPath
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
        remoteDir = try c.decodeIfPresent(String.self, forKey: .remoteDir) ?? ""
        localDir = try c.decodeIfPresent(String.self, forKey: .localDir) ?? ""
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
    var remote_dir: String?
    var local_dir: String?

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
        remote_dir = site.remoteDir
        local_dir = site.localDir
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
            authMode: mode, keyPath: key_path, bucket: bucket, region: region,
            remoteDir: remote_dir ?? "", localDir: local_dir ?? "")
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

    /// Import sessions from a WinSCP.ini file ([Sessions\Name] blocks).
    /// Session names are URL-encoded and may contain "/" folders, which map
    /// straight onto our folder grouping. Returns the number imported.
    func importWinScpIni(_ text: String) throws -> Int {
        struct Pending {
            var name = ""
            var host = ""
            var port: String?
            var user = ""
            var fsProtocol = 0
            var ftpSecure = false
            var keyPath = ""
            var remoteDir = ""
            var localDir = ""
        }

        func decode(_ s: String) -> String {
            s.removingPercentEncoding ?? s
        }

        var current: Pending?

        // Returns 1 when a complete session was flushed into the store.
        // (A local-func + captured-counter version confused the compiler's
        // reachability analysis into a bogus "will never execute" warning.)
        func flush() -> Int {
            guard let p = current, !p.host.isEmpty, !p.name.isEmpty else {
                current = nil
                return 0
            }
            // WinSCP FSProtocol: 5 = FTP (FtpSecure upgrades to FTPS),
            // 7 = S3, everything else is the SSH family → SFTP here.
            let proto: Proto =
                p.fsProtocol == 5 ? (p.ftpSecure ? .ftps : .ftp) : p.fsProtocol == 7 ? .s3 : .sftp
            let port =
                p.port
                ?? {
                    switch proto {
                    case .ftp, .ftps: return "21"
                    case .s3: return "443"
                    default: return "22"
                    }
                }()
            let auth: AuthMode = (!p.keyPath.isEmpty && proto == .sftp) ? .keyFile : .password
            add(
                Site(
                    name: p.name, proto: proto, host: p.host, port: port, user: p.user,
                    authMode: auth, keyPath: p.keyPath,
                    remoteDir: p.remoteDir, localDir: p.localDir))
            current = nil
            return 1
        }

        var count = 0
        for raw in text.split(separator: "\n", omittingEmptySubsequences: false) {
            let line = raw.trimmingCharacters(in: .whitespaces)
            if line.hasPrefix("[") {
                count += flush()
                if line.hasPrefix("[Sessions\\"), line.hasSuffix("]") {
                    let name = String(line.dropFirst("[Sessions\\".count).dropLast())
                    if name != "Default%20Settings" {
                        var pending = Pending()
                        pending.name = decode(name)
                        current = pending
                    }
                }
                continue
            }
            guard current != nil, let eq = line.firstIndex(of: "=") else { continue }
            let key = String(line[..<eq])
            let value = String(line[line.index(after: eq)...])
            switch key {
            case "HostName": current?.host = value
            case "PortNumber": current?.port = value
            case "UserName": current?.user = value
            case "FSProtocol": current?.fsProtocol = Int(value) ?? 0
            case "FtpSecure": current?.ftpSecure = value != "0"
            case "PublicKeyFile": current?.keyPath = decode(value)
            case "RemoteDirectory": current?.remoteDir = decode(value)
            case "LocalDirectory": current?.localDir = decode(value)
            default: break
            }
        }
        count += flush()
        guard count > 0 else {
            throw CoreError(message: "no [Sessions\\…] entries found — is this a WinSCP.ini?")
        }
        return count
    }

    /// Import hosts from an OpenSSH `~/.ssh/config`. Each concrete `Host` alias
    /// (wildcards skipped) becomes an SFTP site grouped under "SSH/", using its
    /// HostName/User/Port/IdentityFile. Returns the number imported.
    func importSshConfig(_ text: String) throws -> Int {
        struct Block {
            var aliases: [String] = []
            var hostName = ""
            var user = ""
            var port = ""
            var identityFile = ""
        }

        // "Key Value" or "Key=Value"; keys are case-insensitive, # comments.
        func parseLine(_ raw: String) -> (key: String, value: String)? {
            var line = raw
            if let hash = line.firstIndex(of: "#") { line = String(line[..<hash]) }
            line = line.trimmingCharacters(in: .whitespaces)
            guard !line.isEmpty else { return nil }
            guard let sep = line.firstIndex(where: { $0 == " " || $0 == "\t" || $0 == "=" })
            else { return (line.lowercased(), "") }
            let key = String(line[..<sep]).lowercased()
            var value = String(line[line.index(after: sep)...])
                .trimmingCharacters(in: CharacterSet(charactersIn: " \t="))
            if value.count >= 2, value.hasPrefix("\""), value.hasSuffix("\"") {
                value = String(value.dropFirst().dropLast())
            }
            return (key, value)
        }

        var blocks: [Block] = []
        var current: Block?
        for raw in text.split(separator: "\n", omittingEmptySubsequences: false) {
            guard let (key, value) = parseLine(String(raw)) else { continue }
            if key == "host" {
                if let c = current { blocks.append(c) }
                var b = Block()
                b.aliases = value.split(whereSeparator: { $0 == " " || $0 == "\t" }).map(String.init)
                current = b
            } else if current != nil {
                switch key {
                case "hostname": current?.hostName = value
                case "user": current?.user = value
                case "port": current?.port = value
                case "identityfile":
                    if current?.identityFile.isEmpty ?? false { current?.identityFile = value }
                default: break
                }
            }
        }
        if let c = current { blocks.append(c) }

        var count = 0
        for b in blocks {
            for alias in b.aliases where !alias.contains("*") && !alias.contains("?") {
                let host = b.hostName.isEmpty ? alias : b.hostName
                guard !host.isEmpty else { continue }
                let key = b.identityFile.isEmpty
                    ? "" : (b.identityFile as NSString).expandingTildeInPath
                add(
                    Site(
                        name: "SSH/\(alias)", proto: .sftp, host: host,
                        port: b.port.isEmpty ? "22" : b.port, user: b.user,
                        authMode: key.isEmpty ? .agent : .keyFile, keyPath: key))
                count += 1
            }
        }
        guard count > 0 else {
            throw CoreError(message: "no Host entries found in the SSH config")
        }
        return count
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
