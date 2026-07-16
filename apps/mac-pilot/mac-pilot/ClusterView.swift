//
//  ClusterView.swift
//  mac-pilot
//
//  Cluster-wide views: one tab with a sub-picker toggling between
//  "Ensemble" (workers + molecules grouped by status, per galaxy) and
//  "Peek" (monospaced fractal snapshot at city / building / skin scale).
//  Both panels shell out to the cs-api endpoints `/ensemble` and
//  `/peek` — no local scan, the server is the single source of truth.
//

import SwiftUI
import Foundation

/// Sub-views inside the single Cluster tab.
enum ClusterPane: String, CaseIterable, Identifiable {
    case ensemble = "Ensemble"
    case peek     = "Peek"
    var id: String { rawValue }
}

// MARK: - API payload types

/// Row returned by `GET /ensemble` for each galaxy.
struct ClusterGalaxyBlock: Decodable, Identifiable {
    var id: String { name }
    let name: String
    let path: String
    let workers: [ClusterWorker]
    let workerCount: Int
    let moleculeGroups: [ClusterMoleculeGroup]
    let totalMolecules: Int

    enum CodingKeys: String, CodingKey {
        case name
        case path
        case workers
        case workerCount = "worker_count"
        case moleculeGroups = "molecule_groups"
        case totalMolecules = "total_molecules"
    }
}

struct ClusterWorker: Decodable, Identifiable {
    var id: String { name + "@" + galaxy }
    let name: String
    let galaxy: String
    let role: String?
    let status: String?
    let moleculeId: String?
    let live: Bool

    enum CodingKeys: String, CodingKey {
        case name, galaxy, role, status, live
        case moleculeId = "molecule_id"
    }
}

struct ClusterMoleculeGroup: Decodable, Identifiable {
    var id: String { status }
    let status: String
    let total: Int
    let sample: [ClusterMoleculeRow]
}

struct ClusterMoleculeRow: Decodable, Identifiable {
    let id: String
    let kind: String
    let galaxy: String
    let status: String
    let topic: String?
    let tags: [String]
    let updatedAt: String?
    let formula: String

    enum CodingKeys: String, CodingKey {
        case id, kind, galaxy, status, topic, tags, formula
        case updatedAt = "updated_at"
    }
}

struct ClusterEnsembleResponse: Decodable {
    let scope: String
    let galaxies: [ClusterGalaxyBlock]
    let totals: ClusterTotals
}

struct ClusterTotals: Decodable {
    let galaxies: Int
    let workers: Int
    let molecules: Int
}

struct ClusterPeekResponse: Decodable {
    let scale: String
    let text: String
}

// MARK: - cs-api HTTP client

/// Error surfaced by the cluster HTTP client.
enum ClusterAPIError: LocalizedError, Equatable {
    case badURL
    case http(Int)
    case decoding

    var errorDescription: String? {
        switch self {
        case .badURL:      return "URL cs-api invalide."
        case .http(let c): return "cs-api HTTP \(c)"
        case .decoding:    return "Réponse cs-api invalide (JSON)."
        }
    }
}

/// Thin HTTP client talking to a local cs-api (default 127.0.0.1:4222).
/// Kept out of `CosmonBridge` so the ClusterView file remains decoupled
/// from the session / inbox plumbing and can evolve independently.
enum ClusterAPI {
    /// cs-api base URL. Allow the operator to override at runtime via
    /// `CS_API_URL` so a mac-pilot pointing at a remote Mac (over
    /// Tailscale) still works.
    static var baseURL: URL {
        if let raw = ProcessInfo.processInfo.environment["CS_API_URL"],
           let url = URL(string: raw) {
            return url
        }
        return URL(string: "http://127.0.0.1:4222")!
    }

    static func ensemble(scope: String = "local") async throws -> ClusterEnsembleResponse {
        try await get(path: "/ensemble", query: [("scope", scope)])
    }

    static func peek(scale: String, focus: String?) async throws -> ClusterPeekResponse {
        var q: [(String, String)] = [("scale", scale)]
        if let f = focus, !f.isEmpty {
            q.append(("focus", f))
        }
        return try await get(path: "/peek", query: q)
    }

    private static func get<R: Decodable>(path: String, query: [(String, String)]) async throws -> R {
        var comp = URLComponents(url: baseURL, resolvingAgainstBaseURL: false) ?? URLComponents()
        comp.path = path
        comp.queryItems = query.map { URLQueryItem(name: $0.0, value: $0.1) }
        guard let url = comp.url else { throw ClusterAPIError.badURL }
        var req = URLRequest(url: url)
        req.timeoutInterval = 10
        req.setValue("application/json", forHTTPHeaderField: "Accept")
        let (data, resp) = try await URLSession.shared.data(for: req)
        guard let http = resp as? HTTPURLResponse else { throw ClusterAPIError.decoding }
        guard (200..<300).contains(http.statusCode) else {
            throw ClusterAPIError.http(http.statusCode)
        }
        do {
            return try JSONDecoder().decode(R.self, from: data)
        } catch {
            throw ClusterAPIError.decoding
        }
    }
}

// MARK: - View models

@MainActor
final class ClusterEnsembleModel: ObservableObject {
    @Published private(set) var response: ClusterEnsembleResponse?
    @Published private(set) var lastError: String?
    @Published private(set) var lastRefresh: Date?

    private var timer: Timer?

    func startPolling(every seconds: TimeInterval = 10) {
        stopPolling()
        let t = Timer.scheduledTimer(withTimeInterval: seconds, repeats: true) { [weak self] _ in
            Task { @MainActor in await self?.refresh() }
        }
        RunLoop.main.add(t, forMode: .common)
        timer = t
    }

    func stopPolling() {
        timer?.invalidate()
        timer = nil
    }

    func refresh() async {
        do {
            response = try await ClusterAPI.ensemble()
            lastError = nil
            lastRefresh = Date()
        } catch {
            lastError = (error as? LocalizedError)?.errorDescription ?? error.localizedDescription
        }
    }
}

@MainActor
final class ClusterPeekModel: ObservableObject {
    @Published var scale: String = "building" {
        didSet { Task { await refresh() } }
    }
    @Published var focus: String = "" {
        didSet { Task { await refresh() } }
    }
    @Published private(set) var text: String = ""
    @Published private(set) var lastError: String?

    private var timer: Timer?

    func startPolling(every seconds: TimeInterval = 10) {
        stopPolling()
        let t = Timer.scheduledTimer(withTimeInterval: seconds, repeats: true) { [weak self] _ in
            Task { @MainActor in await self?.refresh() }
        }
        RunLoop.main.add(t, forMode: .common)
        timer = t
    }

    func stopPolling() {
        timer?.invalidate()
        timer = nil
    }

    func refresh() async {
        do {
            let f = focus.trimmingCharacters(in: .whitespaces)
            let resp = try await ClusterAPI.peek(scale: scale, focus: f.isEmpty ? nil : f)
            text = resp.text
            lastError = nil
        } catch {
            lastError = (error as? LocalizedError)?.errorDescription ?? error.localizedDescription
        }
    }
}

// MARK: - ClusterView

struct ClusterView: View {
    @StateObject private var ensembleModel = ClusterEnsembleModel()
    @StateObject private var peekModel = ClusterPeekModel()
    @State private var pane: ClusterPane = .ensemble

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Picker("", selection: $pane) {
                ForEach(ClusterPane.allCases) { p in
                    Text(p.rawValue).tag(p)
                }
            }
            .pickerStyle(.segmented)
            .padding(.horizontal, 12)

            Divider()

            Group {
                switch pane {
                case .ensemble:
                    ClusterEnsemblePane(model: ensembleModel)
                case .peek:
                    ClusterPeekPane(model: peekModel)
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        .task {
            await ensembleModel.refresh()
            ensembleModel.startPolling()
            await peekModel.refresh()
            peekModel.startPolling()
        }
        .onDisappear {
            ensembleModel.stopPolling()
            peekModel.stopPolling()
        }
    }
}

private struct ClusterEnsemblePane: View {
    @ObservedObject var model: ClusterEnsembleModel

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 10) {
                if let err = model.lastError, model.response == nil {
                    Text(err).font(.footnote).foregroundColor(.red)
                } else if let r = model.response {
                    headerSummary(r)
                    Divider()
                    ForEach(r.galaxies) { g in
                        galaxySection(g)
                    }
                } else {
                    Text("Chargement…").font(.footnote).foregroundColor(.secondary)
                }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
        }
    }

    @ViewBuilder
    private func headerSummary(_ r: ClusterEnsembleResponse) -> some View {
        HStack(spacing: 8) {
            Text("Cluster").font(.caption).foregroundColor(.secondary)
            Text("\(r.totals.galaxies) galaxies")
                .font(.caption2.monospaced())
            Text("\(r.totals.workers) wkrs")
                .font(.caption2.monospaced())
            Text("\(r.totals.molecules) mols")
                .font(.caption2.monospaced())
            Spacer()
        }
    }

    @ViewBuilder
    private func galaxySection(_ g: ClusterGalaxyBlock) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack {
                Text(g.name).font(.caption.weight(.semibold))
                Spacer()
                Text("\(g.workerCount)w · \(g.totalMolecules)m")
                    .font(.caption2.monospaced())
                    .foregroundColor(.secondary)
            }
            ForEach(g.moleculeGroups) { group in
                HStack(spacing: 8) {
                    Text(group.status)
                        .font(.caption2.monospaced())
                        .frame(width: 80, alignment: .leading)
                        .foregroundColor(.secondary)
                    Text("\(group.total)")
                        .font(.caption2.monospaced().bold())
                    Spacer()
                }
                .padding(.leading, 6)
            }
        }
    }
}

private struct ClusterPeekPane: View {
    @ObservedObject var model: ClusterPeekModel

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 4) {
                Picker("", selection: $model.scale) {
                    Text("city").tag("city")
                    Text("building").tag("building")
                    Text("skin").tag("skin")
                }
                .pickerStyle(.segmented)
                .frame(width: 200)

                TextField("focus (galaxy ou mol_id)", text: $model.focus)
                    .textFieldStyle(.roundedBorder)
                    .font(.caption)

                Spacer()
            }
            .padding(.horizontal, 12)

            if let err = model.lastError, model.text.isEmpty {
                Text(err)
                    .font(.footnote)
                    .foregroundColor(.red)
                    .padding(.horizontal, 12)
            }

            ScrollView([.vertical, .horizontal]) {
                Text(model.text.isEmpty ? "Chargement…" : model.text)
                    .font(.system(.caption2, design: .monospaced))
                    .textSelection(.enabled)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 6)
            }
        }
    }
}

#Preview {
    ClusterView()
        .frame(width: 340, height: 500)
}
