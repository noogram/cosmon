import Foundation

/// Errors surfaced by the cs-api client.
public enum CosmonAPIError: LocalizedError, Equatable {
    case notConnected
    case noSessionOpen
    case sessionAlreadyOpen
    case serverError(String)
    case decodingFailed
    case invalidURL
    case notFound
    case notImplemented(String)

    public var errorDescription: String? {
        switch self {
        case .notConnected:
            return "cs-api injoignable."
        case .noSessionOpen:
            return "Aucune session ouverte."
        case .sessionAlreadyOpen:
            return "Une session est déjà ouverte."
        case .serverError(let msg):
            return "cs-api: \(msg)"
        case .decodingFailed:
            return "Réponse cs-api invalide."
        case .invalidURL:
            return "URL cs-api invalide."
        case .notFound:
            return "Ressource introuvable."
        case .notImplemented(let what):
            return "Non implémenté: \(what)"
        }
    }
}

/// Abstract client surface — lets us swap in a mock for simulator/preview work
/// while cs-api (task-20260422-b031) is still being built.
public protocol CosmonAPIProtocol {
    // --- Session (v0) ---
    func start() async throws -> SessionID
    func note(_ text: String, tag: String?) async throws
    func end() async throws -> Seal
    func current() async throws -> SessionState
    func healthz() async throws -> HealthzResponse

    // --- Whispers (v1) ---
    func listWhispers(limit: Int) async throws -> [Whisper]
    func archiveWhisper(id: String) async throws
    func sparkWhisper(id: String, text: String?, nucleon: String?) async throws -> String

    // --- Session promotion (v1) ---
    /// Promote the named session note into a `spark` molecule. Mirror of
    /// `cs session promote <note_ts>` on the mac side. Returns the new
    /// spark id on success. The HTTP live client requires cs-api to
    /// expose a `/session/{id}/promote` endpoint — until it does, the
    /// live client returns [`CosmonAPIError.notImplemented`]. tenant_auditor's
    /// SSH-over-Blink path to `cs` on the Mac remains the primary
    /// workflow on iOS; this method is for the native UI path once
    /// cs-api catches up.
    func promoteNote(sessionID: String, noteTimestamp: String) async throws -> String

    // --- Inbox (v1) ---
    func listInbox(status: String?, limit: Int?) async throws -> [MoleculeSummary]

    // --- Galaxies (v1) ---
    func listGalaxies() async throws -> [Galaxy]

    // --- Cluster (v1) ---
    /// Cluster-wide aggregation: workers + molecule groups per galaxy.
    func ensemble(scope: String) async throws -> ClusterEnsembleResponse

    /// Monospaced fractal snapshot at the given scale (+ optional focus).
    func peek(scale: String, focus: String?) async throws -> ClusterPeekResponse
}

/// Default implementations for API methods that `MotionView`, the
/// Inbox tackle/tag actions, and the cluster probe in `App.swift`
/// were merged against but which never got full transport wiring on
/// ios-pilot. The stubs throw `notImplemented` so the build compiles
/// and the feature surfaces fail loudly at runtime instead of
/// silently no-oping. A follow-up bead (`temp:warm`) owns the
/// proper implementation — see the ios-pilot drift note in
/// `docs/guides/markdown-rendering.md`.
public extension CosmonAPIProtocol {
    func tackleMolecule(id: String) async throws {
        throw CosmonAPIError.serverError("tackleMolecule not implemented on this client")
    }

    func tagMolecule(id: String, add: [String], remove: [String]) async throws {
        throw CosmonAPIError.serverError("tagMolecule not implemented on this client")
    }

    func fetchCluster() async throws -> ClusterConfigResolved? {
        nil
    }
}

/// Resolved cluster configuration — stub type consumed by the
/// first-launch probe in `App.swift`. Paired with the
/// `fetchCluster` default above; the real type will replace this
/// once the cluster-config endpoint ships on ios-pilot.
public struct ClusterConfigResolved {
    public let csApiBaseURL: URL?
    public init(csApiBaseURL: URL?) { self.csApiBaseURL = csApiBaseURL }
}

/// Live HTTP client for cs-api. Reads `baseURL` from UserDefaults at call time,
/// so edits in SettingsView take effect without a relaunch.
public final class CosmonAPI: CosmonAPIProtocol {
    public static let shared = CosmonAPI()

    public static let defaultsKey = "cs_api_url"
    public static let fallbackURL = "http://192.0.2.10:4222"

    private let session: URLSession
    private let decoder: JSONDecoder
    private let encoder: JSONEncoder

    public init(session: URLSession = .shared) {
        self.session = session
        self.decoder = JSONDecoder()
        self.encoder = JSONEncoder()
    }

    /// Resolved base URL. Stored as a computed property so that a change in
    /// Settings is picked up on the next call without reloading state.
    public static var baseURL: URL {
        let raw = UserDefaults.standard.string(forKey: defaultsKey) ?? fallbackURL
        return URL(string: raw) ?? URL(string: fallbackURL)!
    }

    public func start() async throws -> SessionID {
        struct StartResponse: Decodable {
            let session_id: String
        }
        let res: StartResponse = try await post(path: "/session/start", body: Optional<EmptyBody>.none)
        return SessionID(res.session_id)
    }

    public func note(_ text: String, tag: String?) async throws {
        struct NoteBody: Encodable {
            let text: String
            let tag: String?
        }
        struct NoteResponse: Decodable {
            let ok: Bool
        }
        let _: NoteResponse = try await post(path: "/session/note",
                                             body: NoteBody(text: text, tag: tag))
    }

    public func end() async throws -> Seal {
        try await post(path: "/session/end", body: Optional<EmptyBody>.none)
    }

    public func current() async throws -> SessionState {
        try await get(path: "/session/current")
    }

    public func healthz() async throws -> HealthzResponse {
        try await get(path: "/healthz")
    }

    // MARK: - Whispers

    public func listWhispers(limit: Int = 50) async throws -> [Whisper] {
        struct Response: Decodable { let whispers: [Whisper] }
        let clamped = max(1, min(500, limit))
        let res: Response = try await get(path: "/whispers?limit=\(clamped)")
        return res.whispers
    }

    public func archiveWhisper(id: String) async throws {
        struct Response: Decodable { let ok: Bool }
        let _: Response = try await post(
            path: "/whispers/\(encodePathSegment(id))/archive",
            body: Optional<EmptyBody>.none
        )
    }

    public func sparkWhisper(id: String, text: String? = nil, nucleon: String? = nil) async throws -> String {
        struct Body: Encodable {
            let text: String?
            let nucleon: String?
        }
        struct Response: Decodable {
            let ok: Bool
            let spark: SparkBody?
            struct SparkBody: Decodable {
                let id: String?
            }
        }
        let res: Response = try await post(
            path: "/whispers/\(encodePathSegment(id))/spark",
            body: Body(text: text, nucleon: nucleon)
        )
        return res.spark?.id ?? ""
    }

    public func promoteNote(sessionID: String, noteTimestamp: String) async throws -> String {
        // Expected cs-api endpoint (not yet implemented):
        //   POST /session/{sid}/promote  body: {"note_ts": "HH:MM:SS"}
        //   → {"ok": true, "spark": {"id": "spark-..."}}
        // When cs-api gains the route, replace this body with a real
        // `post(...)` call. Until then, iOS-native promotion is
        // explicitly unsupported — the operator should use `cs session
        // promote` over SSH (Blink) or run the LaunchAgent on the mac.
        struct Body: Encodable { let note_ts: String }
        struct Response: Decodable {
            let ok: Bool
            let spark: SparkBody?
            struct SparkBody: Decodable { let id: String? }
        }
        do {
            let res: Response = try await post(
                path: "/session/\(encodePathSegment(sessionID))/promote",
                body: Body(note_ts: noteTimestamp)
            )
            return res.spark?.id ?? ""
        } catch CosmonAPIError.notFound {
            throw CosmonAPIError.notImplemented(
                "/session/{id}/promote — ssh en local avec `cs session promote`"
            )
        }
    }

    // MARK: - Inbox

    public func listInbox(status: String? = "pending,running", limit: Int? = nil) async throws -> [MoleculeSummary] {
        struct Response: Decodable { let molecules: [MoleculeSummary] }
        var query: [String] = []
        if let s = status, !s.isEmpty {
            query.append("status=\(encodeQueryValue(s))")
        }
        if let l = limit {
            query.append("limit=\(l)")
        }
        let q = query.isEmpty ? "" : "?" + query.joined(separator: "&")
        let res: Response = try await get(path: "/inbox\(q)")
        return res.molecules
    }

    // MARK: - Galaxies

    public func listGalaxies() async throws -> [Galaxy] {
        struct Response: Decodable { let galaxies: [Galaxy] }
        let res: Response = try await get(path: "/galaxies")
        return res.galaxies
    }

    // MARK: - Cluster

    public func ensemble(scope: String = "local") async throws -> ClusterEnsembleResponse {
        let q = "?scope=\(encodeQueryValue(scope))"
        return try await get(path: "/ensemble\(q)")
    }

    public func peek(scale: String, focus: String? = nil) async throws -> ClusterPeekResponse {
        var items: [String] = ["scale=\(encodeQueryValue(scale))"]
        if let f = focus, !f.isEmpty {
            items.append("focus=\(encodeQueryValue(f))")
        }
        let q = "?" + items.joined(separator: "&")
        return try await get(path: "/peek\(q)")
    }

    // MARK: - HTTP helpers

    private struct EmptyBody: Encodable {}

    private struct ErrorBody: Decodable {
        let error: String?
    }

    private func encodePathSegment(_ s: String) -> String {
        s.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed) ?? s
    }

    private func encodeQueryValue(_ s: String) -> String {
        s.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed) ?? s
    }

    private func makeRequest(_ path: String, method: String) throws -> URLRequest {
        // `path` may embed a query string, so we cannot use
        // `appendingPathComponent` (it would percent-encode the `?`).
        guard let url = URL(string: path, relativeTo: Self.baseURL)?.absoluteURL else {
            throw CosmonAPIError.invalidURL
        }
        var req = URLRequest(url: url)
        req.httpMethod = method
        req.timeoutInterval = 10
        req.setValue("application/json", forHTTPHeaderField: "Accept")
        return req
    }

    private func get<R: Decodable>(path: String) async throws -> R {
        let req = try makeRequest(path, method: "GET")
        return try await run(req)
    }

    private func post<B: Encodable, R: Decodable>(path: String, body: B?) async throws -> R {
        var req = try makeRequest(path, method: "POST")
        if let body {
            req.setValue("application/json", forHTTPHeaderField: "Content-Type")
            req.httpBody = try encoder.encode(body)
        }
        return try await run(req)
    }

    private func run<R: Decodable>(_ request: URLRequest) async throws -> R {
        let data: Data
        let response: URLResponse
        do {
            (data, response) = try await session.data(for: request)
        } catch {
            throw CosmonAPIError.notConnected
        }
        guard let http = response as? HTTPURLResponse else {
            throw CosmonAPIError.serverError("invalid response")
        }
        if (200..<300).contains(http.statusCode) {
            do {
                return try decoder.decode(R.self, from: data)
            } catch {
                throw CosmonAPIError.decodingFailed
            }
        }
        let body = try? decoder.decode(ErrorBody.self, from: data)
        let msg = body?.error ?? "HTTP \(http.statusCode)"
        switch (http.statusCode, msg) {
        case (_, "session already open"):
            throw CosmonAPIError.sessionAlreadyOpen
        case (_, "no session open"):
            throw CosmonAPIError.noSessionOpen
        case (404, _):
            throw CosmonAPIError.notFound
        default:
            throw CosmonAPIError.serverError(msg)
        }
    }
}

/// Deterministic, in-memory client used for previews, UI tests, and local
/// iteration while cs-api is unavailable. Behaves like a well-formed server.
public final class MockCosmonAPI: CosmonAPIProtocol, @unchecked Sendable {
    private let queue = DispatchQueue(label: "mock-cosmon-api")
    private var state: SessionState = SessionState(sessionID: nil, galaxy: "cosmon", notes: [])
    private var whispers: [Whisper] = MockCosmonAPI.seedWhispers()
    private var inbox: [MoleculeSummary] = MockCosmonAPI.seedInbox()
    private let galaxies: [Galaxy] = MockCosmonAPI.seedGalaxies()

    public init(seeded: SessionState? = nil) {
        if let seeded {
            self.state = seeded
        }
    }

    public func start() async throws -> SessionID {
        try await Task.sleep(nanoseconds: 80_000_000)
        return try queue.sync {
            if state.sessionID != nil { throw CosmonAPIError.sessionAlreadyOpen }
            let id = SessionID("session-\(isoNow())")
            state = SessionState(sessionID: id, galaxy: state.galaxy ?? "cosmon", notes: [])
            return id
        }
    }

    public func note(_ text: String, tag: String?) async throws {
        try await Task.sleep(nanoseconds: 40_000_000)
        try queue.sync {
            guard state.sessionID != nil else { throw CosmonAPIError.noSessionOpen }
            let n = Note(ts: isoNow(), text: text, tag: tag)
            state = SessionState(sessionID: state.sessionID, galaxy: state.galaxy, notes: state.notes + [n])
        }
    }

    public func end() async throws -> Seal {
        try await Task.sleep(nanoseconds: 80_000_000)
        return try queue.sync {
            guard state.sessionID != nil else { throw CosmonAPIError.noSessionOpen }
            let count = state.notes.count
            let seal = Seal(seal: "blake3:mock-\(count)", noteCount: count)
            state = SessionState(sessionID: nil, galaxy: state.galaxy, notes: [])
            return seal
        }
    }

    public func current() async throws -> SessionState {
        try await Task.sleep(nanoseconds: 20_000_000)
        return queue.sync { state }
    }

    public func healthz() async throws -> HealthzResponse {
        HealthzResponse(ok: true, csBinary: "/mock/cs", version: "mock-0.1.0")
    }

    // --- Whispers ---

    public func listWhispers(limit: Int) async throws -> [Whisper] {
        try await Task.sleep(nanoseconds: 40_000_000)
        return queue.sync { Array(whispers.prefix(max(1, min(500, limit)))) }
    }

    public func archiveWhisper(id: String) async throws {
        try await Task.sleep(nanoseconds: 30_000_000)
        try queue.sync {
            guard whispers.contains(where: { $0.wid == id }) else {
                throw CosmonAPIError.notFound
            }
            whispers.removeAll { $0.wid == id }
        }
    }

    public func sparkWhisper(id: String, text: String?, nucleon: String?) async throws -> String {
        try await Task.sleep(nanoseconds: 60_000_000)
        return try queue.sync {
            guard let idx = whispers.firstIndex(where: { $0.wid == id }) else {
                throw CosmonAPIError.notFound
            }
            whispers.remove(at: idx)
            return "idea-20260423-mock"
        }
    }

    public func promoteNote(sessionID: String, noteTimestamp: String) async throws -> String {
        try await Task.sleep(nanoseconds: 40_000_000)
        return queue.sync {
            // Mock is generous — pretend the promotion worked so the
            // UI flow (button → disabled state + badge) is exercisable
            // from the simulator without a live backend.
            return "spark-20260423-mock"
        }
    }

    // --- Inbox ---

    public func listInbox(status: String?, limit: Int?) async throws -> [MoleculeSummary] {
        try await Task.sleep(nanoseconds: 40_000_000)
        return queue.sync {
            let filter: Set<String>? = (status?.isEmpty == false && status?.lowercased() != "all")
                ? Set((status ?? "").split(separator: ",").map { $0.trimmingCharacters(in: .whitespaces).lowercased() })
                : nil
            let matches = inbox.filter { m in
                guard let filter else { return true }
                return filter.contains(m.status)
            }
            if let limit { return Array(matches.prefix(limit)) }
            return matches
        }
    }

    // --- Galaxies ---

    public func listGalaxies() async throws -> [Galaxy] {
        try await Task.sleep(nanoseconds: 20_000_000)
        return galaxies
    }

    // --- Cluster ---

    public func ensemble(scope: String) async throws -> ClusterEnsembleResponse {
        try await Task.sleep(nanoseconds: 30_000_000)
        let g = queue.sync { galaxies }
        let blocks: [ClusterGalaxyBlock] = g.map { gal in
            ClusterGalaxyBlock(
                name: gal.name,
                path: gal.path,
                workers: [],
                workerCount: 0,
                moleculeGroups: [
                    ClusterMoleculeGroup(status: "pending", total: gal.pendingCount, sample: []),
                    ClusterMoleculeGroup(status: "running", total: gal.runningCount, sample: []),
                ],
                totalMolecules: gal.pendingCount + gal.runningCount
            )
        }
        return ClusterEnsembleResponse(
            scope: scope,
            galaxies: blocks,
            totals: ClusterTotals(
                galaxies: blocks.count,
                workers: 0,
                molecules: blocks.reduce(0) { $0 + $1.totalMolecules }
            )
        )
    }

    public func peek(scale: String, focus: String?) async throws -> ClusterPeekResponse {
        try await Task.sleep(nanoseconds: 20_000_000)
        let text: String
        switch scale {
        case "city":     text = "COSMON CLUSTER — CITY VIEW (mock)\n  cosmon   5 pending  2 running\n  mailroom   1 pending"
        case "skin":     text = "COSMON CLUSTER — SKIN VIEW (mock)\n(focus=\(focus ?? "-"))"
        default:         text = "COSMON CLUSTER — BUILDING VIEW (mock)\n▸ cosmon  (0 workers, 5 molecules)"
        }
        return ClusterPeekResponse(scale: scale, text: text)
    }

    // --- Seed data ---

    private static func seedWhispers() -> [Whisper] {
        let now = Date()
        let iso = ISO8601DateFormatter()
        iso.formatOptions = [.withInternetDateTime]
        return [
            Whisper(
                wid: "1776891587880-mock0",
                roomID: "!room:matrix.org",
                senderNucleonID: "you",
                senderMxID: "@you:matrix.org",
                receivedAt: iso.string(from: now.addingTimeInterval(-60)),
                body: "Test whisper mock — tape-moi en task si besoin.",
                path: "/mock/whispers/inbox/_room_matrix.org/1776891587880-mock0.md"
            ),
            Whisper(
                wid: "1776891587881-mock1",
                roomID: "!room:matrix.org",
                senderNucleonID: "you",
                senderMxID: "@you:matrix.org",
                receivedAt: iso.string(from: now.addingTimeInterval(-600)),
                body: "Deuxième whisper : relire le guide de peek demain.",
                path: "/mock/whispers/inbox/_room_matrix.org/1776891587881-mock1.md"
            ),
        ]
    }

    private static func seedInbox() -> [MoleculeSummary] {
        [
            MoleculeSummary(
                id: "task-20260422-abcd",
                kind: "task",
                status: "pending",
                topic: "ios-pilot v1",
                tags: ["temp:hot"],
                createdAt: "2026-04-22T10:00:00Z",
                updatedAt: "2026-04-22T11:00:00Z",
                formula: "task-work",
                assignedWorker: nil
            ),
            MoleculeSummary(
                id: "delib-20260422-f6d6",
                kind: "deliberation",
                status: "running",
                topic: "post-Claude-Code pivot",
                tags: ["temp:warm"],
                createdAt: "2026-04-22T09:00:00Z",
                updatedAt: "2026-04-22T12:30:00Z",
                formula: "deep-think",
                assignedWorker: "worker-42"
            ),
            MoleculeSummary(
                id: "idea-20260421-1234",
                kind: "idea",
                status: "pending",
                topic: "Revoir les invariants cockpit",
                tags: [],
                createdAt: "2026-04-21T14:00:00Z",
                updatedAt: "2026-04-21T14:00:00Z",
                formula: "capture",
                assignedWorker: nil
            ),
        ]
    }

    private static func seedGalaxies() -> [Galaxy] {
        [
            Galaxy(name: "cosmon", path: "/mock/galaxies/cosmon",
                   pendingCount: 5, runningCount: 2,
                   lastActivity: "2026-04-22T12:30:00Z"),
            Galaxy(name: "mailroom", path: "/mock/galaxies/mailroom",
                   pendingCount: 1, runningCount: 0,
                   lastActivity: "2026-04-21T10:00:00Z"),
            Galaxy(name: "showroom", path: "/mock/galaxies/showroom",
                   pendingCount: 0, runningCount: 0,
                   lastActivity: nil),
        ]
    }

    private func isoNow() -> String {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return f.string(from: Date())
    }
}

/// Compile-time switch: `DEBUG` builds default to a shared mock so the
/// simulator always has something to talk to. Release builds hit cs-api.
public enum CosmonAPIFactory {
    public static let shared: CosmonAPIProtocol = {
        #if DEBUG
        if ProcessInfo.processInfo.environment["COSMON_USE_MOCK"] == "1" {
            return MockCosmonAPI()
        }
        return CosmonAPI()
        #else
        return CosmonAPI()
        #endif
    }()
}
