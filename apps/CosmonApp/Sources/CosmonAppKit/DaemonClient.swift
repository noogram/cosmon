// SPDX-License-Identifier: MPL-2.0
//
// DaemonClient — narrow async surface over the cosmon-daemon HTTP API.
//
// One actor per cosmon-daemon endpoint set; thin verb adapters over an
// `HTTPTransport` from `AppsTransportHTTP`. The store consumes this
// protocol so tests can plug a `MockDaemonClient` without standing up
// a socket.

import Foundation
import AppsTransportHTTP

public protocol DaemonClient: Sendable {
    func health() async throws -> DaemonHealth
    func listGalaxies() async throws -> [GalaxyRow]
    func listMolecules(galaxy: String, status: String?) async throws -> [MoleculeSummary]
    func moleculeDetail(galaxy: String, id: String) async throws -> MoleculeDetail
    func moleculeLog(galaxy: String, id: String) async throws -> String
    func listFleets() async throws -> [FleetRow]
}

/// Production client — built on top of `HTTPTransport`. The host/port
/// default to the cosmon-daemon canonical bind
/// (`host.example:8790`); override via the initializer for
/// preview / debug.
public actor LiveDaemonClient: DaemonClient {
    public static let defaultPort = 8790

    private let transport: HTTPTransport

    public init(host: String = HTTPTransportConfig.defaultHost, port: Int = LiveDaemonClient.defaultPort) {
        let config = HTTPTransportConfig(host: host, port: port)
        self.transport = HTTPTransport(config: config)
    }

    public init(transport: HTTPTransport) {
        self.transport = transport
    }

    /// Resolve the bind from `Info.plist` (`CockpitHost` / `CockpitPort`),
    /// falling back to `(host.example, 8790)`. Used by the
    /// app target so the canonical port lives in `Info.plist`, not in
    /// Swift code.
    public static func fromInfoPlist(_ bundle: Bundle = .main) -> LiveDaemonClient {
        var config = HTTPTransportConfig.fromInfoPlist(bundle)
        // The shared default in HTTPTransportConfig is 8789 (Verdict).
        // Cosmon-daemon ships on 8790; if the bundle did not explicitly
        // set CockpitPort, surface our default here.
        if bundle.object(forInfoDictionaryKey: "CockpitPort") == nil {
            config.port = LiveDaemonClient.defaultPort
        }
        return LiveDaemonClient(transport: HTTPTransport(config: config))
    }

    public func health() async throws -> DaemonHealth {
        try await transport.get("/v1/health")
    }

    public func listGalaxies() async throws -> [GalaxyRow] {
        let resp: GalaxiesResponse = try await transport.get("/v1/galaxies")
        return resp.galaxies
    }

    public func listMolecules(galaxy: String, status: String?) async throws -> [MoleculeSummary] {
        var path = "/v1/galaxies/\(escape(galaxy))/molecules"
        if let s = status, !s.isEmpty {
            path += "?status=\(escape(s))"
        }
        let resp: MoleculesResponse = try await transport.get(path)
        return resp.molecules
    }

    public func moleculeDetail(galaxy: String, id: String) async throws -> MoleculeDetail {
        try await transport.get("/v1/galaxies/\(escape(galaxy))/molecules/\(escape(id))")
    }

    public func moleculeLog(galaxy: String, id: String) async throws -> String {
        // Use a separate URLSession path: the canonical transport
        // expects JSON. For now, ride a small URLSession round-trip.
        let url = URL(string: "/v1/galaxies/\(escape(galaxy))/molecules/\(escape(id))/log",
                      relativeTo: await transport.config.baseURL)!
        var req = URLRequest(url: url.absoluteURL)
        req.httpMethod = "GET"
        req.timeoutInterval = await transport.config.timeout
        let (data, response) = try await URLSession.shared.data(for: req)
        guard let http = response as? HTTPURLResponse, (200..<300).contains(http.statusCode) else {
            let status = (response as? HTTPURLResponse)?.statusCode ?? -1
            throw HTTPTransportError.unexpectedStatus(status: status, raw: "")
        }
        return String(data: data, encoding: .utf8) ?? ""
    }

    public func listFleets() async throws -> [FleetRow] {
        let resp: FleetsResponse = try await transport.get("/v1/fleets")
        return resp.fleets
    }

    // MARK: - Helpers

    private nonisolated func escape(_ s: String) -> String {
        s.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed) ?? s
    }
}

/// In-memory client used by previews, tests, and `swift test`. Behaves
/// like a healthy cosmon-daemon over a synthetic two-galaxy cluster.
public actor MockDaemonClient: DaemonClient {
    private let galaxies: [GalaxyRow]
    private var moleculesByGalaxy: [String: [MoleculeSummary]]
    private var detailsByGalaxy: [String: [String: MoleculeDetail]]
    private let logsByGalaxy: [String: [String: String]]
    private let fleets: [FleetRow]

    public init() {
        let now = Date()
        let g1 = GalaxyRow(name: "cosmon", path: "/srv/cosmon/cosmon",
                            moleculeCount: 4, runningCount: 2, pendingCount: 1,
                            lastActivity: now)
        let g2 = GalaxyRow(name: "mailroom", path: "/srv/cosmon/mailroom",
                            moleculeCount: 2, runningCount: 1, pendingCount: 1,
                            lastActivity: now.addingTimeInterval(-300))
        self.galaxies = [g1, g2]
        self.fleets = [
            FleetRow(galaxy: "cosmon", workerCount: 4, repoCount: 0, attentionBudget: nil),
            FleetRow(galaxy: "mailroom", workerCount: 2, repoCount: 0, attentionBudget: nil),
        ]
        let mol1 = MoleculeSummary(id: "task-20260426-aaaa", status: "running",
                                    kind: "task", formula: "task-work",
                                    currentStep: 0, totalSteps: 2,
                                    worker: "cosmon-app-aaaa", workerLive: "working",
                                    liveness: "healthy", updatedAt: now)
        let mol2 = MoleculeSummary(id: "task-20260426-bbbb", status: "pending",
                                    kind: "task", formula: "task-work",
                                    currentStep: 0, totalSteps: 2,
                                    worker: nil, workerLive: nil,
                                    liveness: "unknown", updatedAt: now.addingTimeInterval(-120))
        let mol3 = MoleculeSummary(id: "delib-20260426-cccc", status: "running",
                                    kind: "deliberation", formula: "deep-think",
                                    currentStep: 1, totalSteps: 3,
                                    worker: "panel-deep-think", workerLive: "working",
                                    liveness: "healthy", updatedAt: now.addingTimeInterval(-60))
        let mol4 = MoleculeSummary(id: "task-20260426-dddd", status: "completed",
                                    kind: "task", formula: "task-work",
                                    currentStep: 2, totalSteps: 2,
                                    worker: nil, workerLive: nil,
                                    liveness: "unknown", updatedAt: now.addingTimeInterval(-3600))
        self.moleculesByGalaxy = [
            "cosmon": [mol1, mol2, mol3, mol4],
            "mailroom": [mol1, mol2],
        ]
        var details: [String: [String: MoleculeDetail]] = [:]
        var logs: [String: [String: String]] = [:]
        for (galaxy, mols) in moleculesByGalaxy {
            var d: [String: MoleculeDetail] = [:]
            var l: [String: String] = [:]
            for m in mols {
                d[m.id] = MoleculeDetail(
                    galaxy: galaxy, id: m.id, fleetId: "default",
                    status: m.status, kind: m.kind, formula: m.formula,
                    currentStep: m.currentStep, totalSteps: m.totalSteps,
                    worker: m.worker,
                    variables: ["topic": "Mock molecule \(m.id)"],
                    links: [], completedSteps: [], collapseReason: nil,
                    createdAt: m.updatedAt.addingTimeInterval(-1800),
                    updatedAt: m.updatedAt,
                    logTail: "stub log line for \(m.id)\n",
                    logTruncated: false,
                    briefing: "# briefing\n\nMock briefing for \(m.id).\n",
                    tmuxAttachHint: m.worker.map { "tmux -L \(galaxy) attach -t \($0)" }
                )
                l[m.id] = "log line one\nlog line two\nlog line three for \(m.id)\n"
            }
            details[galaxy] = d
            logs[galaxy] = l
        }
        self.detailsByGalaxy = details
        self.logsByGalaxy = logs
    }

    public func health() async throws -> DaemonHealth {
        let running = galaxies.reduce(0) { $0 + $1.runningCount }
        return DaemonHealth(ok: true, service: "cosmon-daemon", version: "0.1.0-mock",
                            galaxiesCount: galaxies.count, moleculesRunning: running)
    }

    public func listGalaxies() async throws -> [GalaxyRow] { galaxies }

    public func listMolecules(galaxy: String, status: String?) async throws -> [MoleculeSummary] {
        let pool = moleculesByGalaxy[galaxy] ?? []
        guard let s = status, !s.isEmpty else { return pool }
        let want = Set(s.split(separator: ",").map { $0.trimmingCharacters(in: .whitespaces).lowercased() })
        return pool.filter { want.contains($0.status) }
    }

    public func moleculeDetail(galaxy: String, id: String) async throws -> MoleculeDetail {
        guard let d = detailsByGalaxy[galaxy]?[id] else {
            throw HTTPTransportError.applicationError(
                status: 404,
                body: ApplicationErrorBody(error: "not found: \(id)", code: "not_found", detail: nil)
            )
        }
        return d
    }

    public func moleculeLog(galaxy: String, id: String) async throws -> String {
        logsByGalaxy[galaxy]?[id] ?? ""
    }

    public func listFleets() async throws -> [FleetRow] { fleets }
}
