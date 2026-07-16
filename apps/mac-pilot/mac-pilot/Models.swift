//
//  Models.swift
//  mac-pilot
//
//  Plain data types shared between CosmonBridge (shell-out + parser) and
//  the SwiftUI popover views. Everything here is pure Swift value types —
//  no I/O, no actors, no environment coupling.
//

import Foundation

/// Opaque identifier of a cosmon session (e.g. `session-2026-04-22T10-31-31Z`).
struct SessionID: Equatable, Hashable, CustomStringConvertible {
    let raw: String
    var description: String { raw }
}

/// A single timestamped note captured inside an open session.
struct Note: Identifiable, Hashable {
    let id: UUID
    let timestamp: String
    let tag: String?
    let text: String

    init(timestamp: String, tag: String?, text: String) {
        self.id = UUID()
        self.timestamp = timestamp
        self.tag = tag
        self.text = text
    }
}

/// Snapshot of the currently open session, as parsed from the markdown file.
struct SessionState: Equatable {
    let sessionID: SessionID
    let startedAt: Date
    let notes: [Note]
}

/// Result of a successful `cs session end`.
struct Seal: Equatable {
    let sessionID: SessionID
    let hash: String
}

// MARK: - Whispers

/// A single whisper parsed from a Matrix ingress `.md` file on disk.
///
/// The file path itself is part of the identity — we use it for the archive
/// move. `frontmatter` is a raw key/value map; the well-known keys
/// (`sender_mxid`, `origin_server_ts`, …) are also exposed as convenience
/// accessors so views don't need to grovel through the dictionary.
struct Whisper: Identifiable, Hashable {
    /// Absolute path of the `.md` file — also serves as the stable identity.
    let url: URL
    /// Room id extracted from the parent directory name (e.g. `_room_matrix.org`).
    let roomDirectoryName: String
    let frontmatter: [String: String]
    let body: String
    /// Received timestamp, derived from `received_at` frontmatter or file mtime.
    let receivedAt: Date

    var id: URL { url }

    var senderNucleonID: String { frontmatter["sender_nucleon_id"] ?? "?" }
    var senderMxID: String { frontmatter["sender_mxid"] ?? "" }
    var roomID: String { frontmatter["room_id"] ?? "" }
    var source: String { frontmatter["source"] ?? "" }
    var msgType: String { frontmatter["msgtype"] ?? "" }

    /// First ~40 characters of the body, single-line.
    var preview: String {
        let flat = body
            .replacingOccurrences(of: "\n", with: " ")
            .trimmingCharacters(in: .whitespacesAndNewlines)
        if flat.count <= 40 { return flat }
        return flat.prefix(40) + "…"
    }
}

// MARK: - Inbox / Molecules

/// Summary row as returned by `cs observe --json` (list mode).
struct MoleculeSummary: Identifiable, Hashable {
    let id: String
    let formula: String
    let status: String
    let worker: String?
    /// Cached tags when available (may be empty on list view — details fetch
    /// fills these in).
    let tags: [String]

    /// Short 4-char suffix (e.g. `20ae`) used in list rendering.
    var shortID: String {
        if let dash = id.lastIndex(of: "-") {
            let start = id.index(after: dash)
            return String(id[start...])
        }
        return id
    }

    /// Molecule kind emoji inferred from the id prefix.
    var kindEmoji: String {
        let prefix = id.split(separator: "-").first.map(String.init) ?? ""
        switch prefix {
        case "idea":          return "💡"
        case "task":          return "🔧"
        case "decision":      return "📐"
        case "issue":         return "🐛"
        case "signal":        return "⚡"
        case "deliberation":  return "🧠"
        case "delib":         return "🧠"
        case "spark":         return "✨"
        case "const":         return "⭐️"
        case "constellation": return "⭐️"
        case "adr":           return "📜"
        default:              return "◻︎"
        }
    }
}

/// Detailed molecule record as returned by `cs observe <id> --json`.
struct MoleculeDetail: Equatable {
    let id: String
    let formula: String
    let status: String
    let topic: String
    let tags: [String]
    let moleculeDir: String?
    let worker: String?
    let createdAt: Date?
    let updatedAt: Date?
    /// Typed DAG edges parsed from the `typed_links` array. Free-form
    /// `Entangled` string links are represented with `target` carrying
    /// arbitrary text (not necessarily a molecule id).
    let typedLinks: [MoleculeTypedLink]
}

/// Typed link between molecules — mirror of the Rust
/// `MoleculeLink` enum (see `crates/cosmon-core/src/interaction.rs`).
///
/// We keep the Swift shape deliberately simple: a relation tag + a
/// single target string. `MergedFrom` expands to multiple links on
/// parse. `TransformedFrom` carries the previous kind as the `target`
/// since there is no molecule id to point at. Unknown relations from
/// future Rust variants fall through as `.entangled(rel)` so the view
/// does not crash on a forward-compat JSON payload.
enum MoleculeTypedLinkRelation: String, Hashable, CaseIterable {
    case blockedBy     = "blocked_by"
    case blocks        = "blocks"
    case refines       = "refines"
    case refinedBy     = "refined_by"
    case decayedFrom   = "decayed_from"
    case decayProduct  = "decay_product"
    case mergedFrom    = "merged_from"
    case mergedInto    = "merged_into"
    case transformedFrom = "transformed_from"
    case entangled     = "entangled"

    /// Fixed display order in the detail pane — mirrors the semantic
    /// hierarchy (what blocks me → what I block → cites → lineage → peer).
    var displayRank: Int {
        switch self {
        case .blockedBy:        return 0
        case .blocks:           return 1
        case .refines:          return 2
        case .refinedBy:        return 3
        case .decayedFrom:      return 4
        case .decayProduct:     return 5
        case .mergedFrom:       return 6
        case .mergedInto:       return 7
        case .transformedFrom:  return 8
        case .entangled:        return 9
        }
    }

    /// Short French label for the group header. Matches the popover's
    /// existing tone (Inbox / Retour / Tackle) so we don't mix languages.
    var header: String {
        switch self {
        case .blockedBy:        return "Bloqué par"
        case .blocks:           return "Bloque"
        case .refines:          return "Raffine"
        case .refinedBy:        return "Raffiné par"
        case .decayedFrom:      return "Décayé de"
        case .decayProduct:     return "Produits de décay"
        case .mergedFrom:       return "Issu du merge"
        case .mergedInto:       return "Fusionné dans"
        case .transformedFrom:  return "Transformé depuis"
        case .entangled:        return "Entrelacé"
        }
    }
}

/// A single typed link with a rendering-friendly shape.
///
/// `target` is the molecule id for the molecule-pointing variants,
/// the free-form string for `Entangled`, and the previous kind for
/// `TransformedFrom`. `targetIsMolecule` tells the view whether the
/// button should switch the Inbox selection (true) or stay passive
/// (false — e.g. external URL, previous kind).
struct MoleculeTypedLink: Identifiable, Hashable {
    let id: UUID
    let relation: MoleculeTypedLinkRelation
    let target: String
    let targetIsMolecule: Bool

    init(relation: MoleculeTypedLinkRelation, target: String, targetIsMolecule: Bool) {
        self.id = UUID()
        self.relation = relation
        self.target = target
        self.targetIsMolecule = targetIsMolecule
    }
}

// MARK: - Galaxies

/// A galaxy discovered under `/srv/cosmon/*/` — any directory containing a
/// `.cosmon/` subdirectory qualifies.
struct Galaxy: Identifiable, Hashable {
    let name: String
    let path: URL
    let pendingCount: Int
    let lastActivity: Date?

    var id: String { path.path }
}

// MARK: - Motion

/// One section of the "molécules en mouvement" cockpit — the live view
/// of what the cluster is doing. Mirrors the JSON returned by
/// `cs motion --json` and `GET /motion` on cs-api.
struct MotionSnapshot: Equatable {
    let timestamp: String
    let window: String
    let galaxiesScanned: [String]
    let workers: [MotionWorker]
    let runningMolecules: [MotionMolecule]
    let recentCommits: [MotionCommit]
    let recentWhispers: [MotionWhisper]
    let recentSparks: [MotionSpark]

    static let empty = MotionSnapshot(
        timestamp: "",
        window: "",
        galaxiesScanned: [],
        workers: [],
        runningMolecules: [],
        recentCommits: [],
        recentWhispers: [],
        recentSparks: []
    )
}

struct MotionWorker: Identifiable, Hashable {
    let name: String
    let galaxy: String
    let moleculeID: String?
    let role: String?
    let status: String?
    let lastHeartbeat: String?
    let costUSD: Double?
    let repo: String?

    var id: String { "\(galaxy)/\(name)" }
}

struct MotionMolecule: Identifiable, Hashable {
    let id: String
    let galaxy: String
    let kind: String
    let currentStep: Int?
    let totalSteps: Int?
    let lastEvolveAt: String?
    let tags: [String]
    let topicPreview: String?
    let assignedWorker: String?

    var stepLabel: String {
        switch (currentStep, totalSteps) {
        case (let s?, let t?): return "step \(s)/\(t)"
        case (let s?, _): return "step \(s)"
        default: return "step ?"
        }
    }
}

struct MotionCommit: Identifiable, Hashable {
    let galaxy: String
    let sha: String
    let subject: String
    let timestamp: String
    let author: String

    var id: String { "\(galaxy)/\(sha)" }
}

struct MotionWhisper: Identifiable, Hashable {
    let id: String
    let galaxy: String
    let senderNucleonID: String?
    let receivedAt: String
    let bodyPreview: String
}

struct MotionSpark: Identifiable, Hashable {
    let id: String
    let galaxy: String
    let createdAt: String
    let topicPreview: String?
    let tags: [String]
}

// MARK: - Errors

/// Domain errors surfaced through `CosmonBridge`. Exit codes 2 and 3 are the
/// cosmon contract (see `cs session --help`) — map them to actionable messages
/// rather than raw `executionFailed`.
enum CosmonError: LocalizedError, Equatable {
    case csNotFound
    case noSessionOpen
    case sessionAlreadyOpen(path: String?)
    case executionFailed(exitCode: Int32, stderr: String)
    case parseFailure(String)

    var errorDescription: String? {
        switch self {
        case .csNotFound:
            return "Binaire `cs` introuvable. Configure `CS_BINARY_PATH` dans le scheme Xcode, ou installe cs via `just install`."
        case .noSessionOpen:
            return "Aucune session ouverte. Démarre une session d'abord."
        case .sessionAlreadyOpen(let path):
            if let path { return "Une session est déjà ouverte : \(path). Ferme-la d'abord." }
            return "Une session est déjà ouverte. Ferme-la d'abord."
        case .executionFailed(let code, let stderr):
            let msg = stderr.trimmingCharacters(in: .whitespacesAndNewlines)
            return "`cs` a échoué (code \(code))" + (msg.isEmpty ? "." : " : \(msg)")
        case .parseFailure(let msg):
            return "Parse : \(msg)"
        }
    }
}
