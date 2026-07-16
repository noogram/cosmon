// SPDX-License-Identifier: MPL-2.0
//
// WireModels — `Decodable` mirror of the cosmon-daemon Rust DTOs.
//
// JSON convention (set by AppsTransportHTTP): snake_case keys, dates as
// seconds-since-1970 doubles. Field names below use camelCase; the
// transport's decoder converts at the boundary, so we never write
// `enum CodingKeys` for the conversion alone — only for the few
// fields whose API name does not map cleanly (e.g. `id`).

import Foundation

public struct DaemonHealth: Codable, Sendable, Equatable {
    public let ok: Bool
    public let service: String
    public let version: String
    public let galaxiesCount: Int
    public let moleculesRunning: Int
}

public struct GalaxyRow: Codable, Sendable, Hashable, Identifiable {
    public var id: String { name }
    public let name: String
    public let path: String
    public let moleculeCount: Int
    public let runningCount: Int
    public let pendingCount: Int
    public let lastActivity: Date?
}

public struct GalaxiesResponse: Codable, Sendable, Equatable {
    public let galaxies: [GalaxyRow]
}

public struct MoleculeSummary: Codable, Sendable, Hashable, Identifiable {
    public let id: String
    public let status: String
    public let kind: String?
    public let formula: String
    public let currentStep: Int
    public let totalSteps: Int
    public let worker: String?
    public let workerLive: String?
    public let liveness: String
    public let updatedAt: Date

    /// 4-char suffix used in compact list rows.
    public var shortID: String {
        if let dash = id.lastIndex(of: "-") {
            return String(id[id.index(after: dash)...])
        }
        return id
    }

    /// Emoji glyph inferred from `kind` (mirrors mac-pilot conventions).
    public var kindGlyph: String {
        switch kind {
        case "task": return "🔧"
        case "idea": return "💡"
        case "decision": return "📐"
        case "issue": return "🐛"
        case "signal": return "⚡"
        case "deliberation", "delib": return "🧠"
        case "spark": return "✨"
        case "constellation", "const": return "⭐️"
        case "adr": return "📜"
        case "absorb": return "🔄"
        default: return "◻︎"
        }
    }
}

public struct MoleculesResponse: Codable, Sendable, Equatable {
    public let galaxy: String
    public let molecules: [MoleculeSummary]
}

public struct MoleculeDetail: Codable, Sendable, Hashable {
    public let galaxy: String
    public let id: String
    public let fleetId: String
    public let status: String
    public let kind: String?
    public let formula: String
    public let currentStep: Int
    public let totalSteps: Int
    public let worker: String?
    public let variables: [String: String]
    public let links: [String]
    public let completedSteps: [String]
    public let collapseReason: String?
    public let createdAt: Date
    public let updatedAt: Date
    public let logTail: String?
    public let logTruncated: Bool
    public let briefing: String?
    public let tmuxAttachHint: String?
}

public struct FleetRow: Codable, Sendable, Hashable, Identifiable {
    public var id: String { galaxy }
    public let galaxy: String
    public let workerCount: Int
    public let repoCount: Int
    public let attentionBudget: Int?
}

public struct FleetsResponse: Codable, Sendable, Equatable {
    public let fleets: [FleetRow]
}
