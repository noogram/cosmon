import Foundation

/// Session identifier returned by cs-api.
///
/// Shape is chosen to mirror what mac-pilot (sibling app) will use — both
/// apps can share this file verbatim in a future SwiftPM package.
public struct SessionID: Codable, Hashable, Equatable {
    public let value: String

    public init(_ value: String) {
        self.value = value
    }
}

/// A single note appended to a live session.
public struct Note: Codable, Hashable, Identifiable {
    public var id: String { ts }
    public let ts: String
    public let text: String
    public let tag: String?

    public init(ts: String, text: String, tag: String? = nil) {
        self.ts = ts
        self.text = text
        self.tag = tag
    }
}

/// Snapshot of the current session on the Mac, as reported by cs-api.
public struct SessionState: Codable, Equatable {
    public let sessionID: SessionID?
    public let galaxy: String?
    public let notes: [Note]

    enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
        case galaxy
        case notes
    }

    public init(sessionID: SessionID?, galaxy: String?, notes: [Note]) {
        self.sessionID = sessionID
        self.galaxy = galaxy
        self.notes = notes
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        if let raw = try c.decodeIfPresent(String.self, forKey: .sessionID) {
            self.sessionID = SessionID(raw)
        } else {
            self.sessionID = nil
        }
        self.galaxy = try c.decodeIfPresent(String.self, forKey: .galaxy)
        self.notes = (try c.decodeIfPresent([Note].self, forKey: .notes)) ?? []
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encodeIfPresent(sessionID?.value, forKey: .sessionID)
        try c.encodeIfPresent(galaxy, forKey: .galaxy)
        try c.encode(notes, forKey: .notes)
    }
}

/// End-of-session seal (BLAKE3 over the concatenated notes).
public struct Seal: Codable, Equatable {
    public let seal: String
    public let noteCount: Int

    enum CodingKeys: String, CodingKey {
        case seal
        case noteCount = "note_count"
    }
}

public struct HealthzResponse: Codable, Equatable {
    public let ok: Bool
    public let csBinary: String?
    public let version: String?

    enum CodingKeys: String, CodingKey {
        case ok
        case csBinary = "cs_binary"
        case version
    }
}

/// A note the user wrote while cs-api was unreachable, queued for later.
public struct PendingNote: Codable, Identifiable, Equatable {
    public let id: UUID
    public let text: String
    public let tag: String?
    public let enqueuedAt: Date

    public init(id: UUID = UUID(), text: String, tag: String?, enqueuedAt: Date = Date()) {
        self.id = id
        self.text = text
        self.tag = tag
        self.enqueuedAt = enqueuedAt
    }
}

// MARK: - Whispers (v1)

/// A single whisper record, as returned by `GET /whispers`.
///
/// Identity is the server-side `id` (filename stem
/// `<origin_ts>-<event_id>` assembled by `cosmon-matrix-tick`). The
/// remaining fields are rendered directly in the detail pane — no
/// extra round-trip is needed to show the body.
public struct Whisper: Codable, Hashable, Identifiable {
    public var id: String { wid }
    /// Opaque whisper identifier — `<origin_ts>-<event_id>` in the
    /// current matrix-tick layout. Keep as `String`; the iOS client
    /// does not parse it.
    public let wid: String
    public let roomID: String
    public let senderNucleonID: String?
    public let senderMxID: String?
    /// ISO-8601 UTC timestamp emitted by `cosmon-matrix-tick`.
    public let receivedAt: String
    public let body: String
    /// Absolute filesystem path on the Mac — kept for the debug log.
    public let path: String?

    enum CodingKeys: String, CodingKey {
        case wid = "id"
        case roomID = "room_id"
        case senderNucleonID = "sender_nucleon_id"
        case senderMxID = "sender_mxid"
        case receivedAt = "received_at"
        case body
        case path
    }

    /// Best-effort parse of `receivedAt` into a `Date`. Falls back to
    /// `Date()` for unparseable strings so empty states still render.
    public var receivedAtDate: Date {
        let iso = ISO8601DateFormatter()
        iso.formatOptions = [.withInternetDateTime]
        if let d = iso.date(from: receivedAt) { return d }
        iso.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        if let d = iso.date(from: receivedAt) { return d }
        return Date()
    }

    /// Truncated single-line preview used in list rows.
    public var preview: String {
        let flat = body
            .replacingOccurrences(of: "\n", with: " ")
            .trimmingCharacters(in: .whitespacesAndNewlines)
        if flat.isEmpty { return "(vide)" }
        if flat.count <= 80 { return flat }
        return String(flat.prefix(80)) + "…"
    }
}

// MARK: - Inbox / Molecules (v1)

/// Summary row returned by `GET /inbox`.
public struct MoleculeSummary: Codable, Hashable, Identifiable {
    public let id: String
    public let kind: String
    public let status: String
    public let topic: String?
    public let tags: [String]
    public let createdAt: String
    public let updatedAt: String
    public let formula: String
    public let assignedWorker: String?

    enum CodingKeys: String, CodingKey {
        case id, kind, status, topic, tags, formula
        case createdAt = "created_at"
        case updatedAt = "updated_at"
        case assignedWorker = "assigned_worker"
    }

    /// Short 4-char suffix used in list rendering.
    public var shortID: String {
        if let dash = id.lastIndex(of: "-") {
            let start = id.index(after: dash)
            return String(id[start...])
        }
        return id
    }

    /// Kind emoji inferred from the id prefix (mirrors mac-pilot).
    public var kindEmoji: String {
        switch kind {
        case "idea":          return "💡"
        case "task":          return "🔧"
        case "decision":      return "📐"
        case "issue":         return "🐛"
        case "signal":        return "⚡"
        case "deliberation":  return "🧠"
        case "delib":         return "🧠"
        case "spark":         return "✨"
        case "const", "constellation": return "⭐️"
        case "adr":           return "📜"
        case "absorb":        return "🔄"
        case "chronlint":     return "📝"
        default:              return "◻︎"
        }
    }

    /// True iff the molecule carries `temp:hot`.
    public var isHot: Bool {
        tags.contains("temp:hot")
    }
}

// MARK: - Galaxies (v1)

/// A galaxy row returned by `GET /galaxies`.
public struct Galaxy: Codable, Hashable, Identifiable {
    public var id: String { path }
    public let name: String
    public let path: String
    public let pendingCount: Int
    public let runningCount: Int
    public let lastActivity: String?

    enum CodingKeys: String, CodingKey {
        case name, path
        case pendingCount = "pending_count"
        case runningCount = "running_count"
        case lastActivity = "last_activity"
    }
}

// MARK: - Cluster (v1)

/// Per-galaxy roll-up returned by `GET /ensemble`.
public struct ClusterGalaxyBlock: Codable, Hashable, Identifiable {
    public var id: String { name }
    public let name: String
    public let path: String
    public let workers: [ClusterWorker]
    public let workerCount: Int
    public let moleculeGroups: [ClusterMoleculeGroup]
    public let totalMolecules: Int

    enum CodingKeys: String, CodingKey {
        case name, path, workers
        case workerCount = "worker_count"
        case moleculeGroups = "molecule_groups"
        case totalMolecules = "total_molecules"
    }
}

/// One worker row inside a galaxy block.
public struct ClusterWorker: Codable, Hashable, Identifiable {
    public var id: String { name + "@" + galaxy }
    public let name: String
    public let galaxy: String
    public let role: String?
    public let status: String?
    public let moleculeId: String?
    public let live: Bool

    enum CodingKeys: String, CodingKey {
        case name, galaxy, role, status, live
        case moleculeId = "molecule_id"
    }
}

/// One status group (running/pending/…) with a capped sample.
public struct ClusterMoleculeGroup: Codable, Hashable, Identifiable {
    public var id: String { status }
    public let status: String
    public let total: Int
    public let sample: [ClusterMoleculeRow]
}

/// One sampled molecule row inside a status group.
public struct ClusterMoleculeRow: Codable, Hashable, Identifiable {
    public let id: String
    public let kind: String
    public let galaxy: String
    public let status: String
    public let topic: String?
    public let tags: [String]
    public let updatedAt: String?
    public let formula: String

    enum CodingKeys: String, CodingKey {
        case id, kind, galaxy, status, topic, tags, formula
        case updatedAt = "updated_at"
    }
}

/// Totals across every galaxy in the cluster.
public struct ClusterTotals: Codable, Hashable {
    public let galaxies: Int
    public let workers: Int
    public let molecules: Int
}

/// Envelope returned by `GET /ensemble`.
public struct ClusterEnsembleResponse: Codable, Hashable {
    public let scope: String
    public let galaxies: [ClusterGalaxyBlock]
    public let totals: ClusterTotals
}

/// Envelope returned by `GET /peek` — monospace text body.
public struct ClusterPeekResponse: Codable, Hashable {
    public let scale: String
    public let text: String
}
