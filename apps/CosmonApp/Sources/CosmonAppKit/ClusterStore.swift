// SPDX-License-Identifier: MPL-2.0
//
// ClusterStore — `@MainActor` `ObservableObject` driving every screen.
//
// Holds three slices of state — galaxies, per-galaxy molecules, fleets —
// and a polling loop (5 s) that refreshes each slice in turn. Polling
// runs while at least one screen is mounted. The store does not own the
// transport; it consumes the `DaemonClient` protocol.

#if canImport(Combine)
import Combine
#endif
import Foundation
import AppsTransportHTTP

#if canImport(SwiftUI)
import SwiftUI

@MainActor
public final class ClusterStore: ObservableObject {
    @Published public private(set) var galaxies: [GalaxyRow] = []
    @Published public private(set) var galaxiesError: String?
    @Published public private(set) var galaxiesLoading: Bool = false

    @Published public private(set) var moleculesByGalaxy: [String: [MoleculeSummary]] = [:]
    @Published public private(set) var moleculesError: [String: String] = [:]
    @Published public private(set) var moleculesLoading: [String: Bool] = [:]

    @Published public private(set) var fleets: [FleetRow] = []

    @Published public private(set) var health: DaemonHealth?
    /// `true` once a poll has succeeded since launch.
    @Published public private(set) var hasReachedDaemon: Bool = false
    /// Last error string from the most recent overall poll.
    @Published public private(set) var lastError: String?

    public let pollInterval: TimeInterval

    private let client: DaemonClient
    private var pollTask: Task<Void, Never>?

    public init(client: DaemonClient, pollInterval: TimeInterval = 5.0) {
        self.client = client
        self.pollInterval = pollInterval
    }

    public func startPolling() {
        pollTask?.cancel()
        pollTask = Task { [weak self] in
            guard let self else { return }
            while !Task.isCancelled {
                await self.refresh()
                try? await Task.sleep(nanoseconds: UInt64(self.pollInterval * 1_000_000_000))
            }
        }
    }

    public func stopPolling() {
        pollTask?.cancel()
        pollTask = nil
    }

    deinit {
        pollTask?.cancel()
    }

    public func refresh() async {
        await refreshGalaxies()
        await refreshFleets()
        await refreshHealth()
        // Refresh the molecules slice for every galaxy we already
        // expanded — fresh galaxies populate on demand from the
        // screens that need them.
        for galaxy in moleculesByGalaxy.keys {
            await refreshMolecules(galaxy: galaxy, status: nil)
        }
    }

    public func refreshGalaxies() async {
        galaxiesLoading = true
        defer { galaxiesLoading = false }
        do {
            self.galaxies = try await client.listGalaxies()
            self.galaxiesError = nil
            self.hasReachedDaemon = true
            self.lastError = nil
        } catch {
            self.galaxiesError = humanize(error)
            self.lastError = humanize(error)
        }
    }

    public func refreshMolecules(galaxy: String, status: String?) async {
        moleculesLoading[galaxy] = true
        defer { moleculesLoading[galaxy] = false }
        do {
            let mols = try await client.listMolecules(galaxy: galaxy, status: status)
            self.moleculesByGalaxy[galaxy] = mols
            self.moleculesError[galaxy] = nil
            self.hasReachedDaemon = true
            self.lastError = nil
        } catch {
            self.moleculesError[galaxy] = humanize(error)
            self.lastError = humanize(error)
        }
    }

    public func refreshFleets() async {
        do {
            self.fleets = try await client.listFleets()
        } catch {
            self.lastError = humanize(error)
        }
    }

    public func refreshHealth() async {
        do {
            self.health = try await client.health()
            self.hasReachedDaemon = true
        } catch {
            self.lastError = humanize(error)
        }
    }

    public func loadDetail(galaxy: String, id: String) async throws -> MoleculeDetail {
        try await client.moleculeDetail(galaxy: galaxy, id: id)
    }

    public func loadLog(galaxy: String, id: String) async throws -> String {
        try await client.moleculeLog(galaxy: galaxy, id: id)
    }

    private nonisolated func humanize(_ error: Error) -> String {
        if let e = error as? HTTPTransportError {
            switch e {
            case .daemonOffline(let reason):
                return "cosmon-daemon hors ligne — \(reason)"
            case .applicationError(_, let body):
                return body.detail ?? body.error
            case .unexpectedStatus(let status, _):
                return "HTTP \(status) inattendu"
            case .protocolMismatch(let reason):
                return "schéma désaligné — \(reason)"
            case .nonHTTPResponse:
                return "réponse non-HTTP"
            case .cancelled:
                return "annulé"
            }
        }
        return "\(error)"
    }
}
#endif
