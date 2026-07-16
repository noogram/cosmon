import Foundation
import SwiftUI

/// Connectivity traffic-light for the header indicator.
public enum ConnectivityStatus: Equatable {
    case unknown
    case connected
    case degraded
    case offline

    public var color: Color {
        switch self {
        case .unknown: return .gray
        case .connected: return .green
        case .degraded: return .orange
        case .offline: return .red
        }
    }
}

/// Single source of truth for the live session, drives SessionView.
@MainActor
public final class SessionStore: ObservableObject {
    @Published public private(set) var state: SessionState = SessionState(
        sessionID: nil, galaxy: nil, notes: [])
    @Published public private(set) var connectivity: ConnectivityStatus = .unknown
    @Published public private(set) var lastError: String?
    @Published public private(set) var isBusy: Bool = false
    @Published public private(set) var pending: [PendingNote] = []

    /// Set of note timestamps (`ts`) known to have been promoted into a
    /// `spark` molecule during this UI session. Used to hide the
    /// "Promouvoir en spark" button on an already-promoted row. This is
    /// a UI-local hint — the authoritative idempotence record lives on
    /// the mac side under `.cosmon/state/sessions/.promoted/`. When
    /// cs-api gains a `/session/{id}/promoted` GET, we will hydrate
    /// this set on refresh instead of tracking it client-side only.
    @Published public private(set) var promotedTimestamps: Set<String> = []

    /// Short-lived map `note_ts → spark_id` produced by the most recent
    /// `promote(_:)` call. The UI reads it to render a `spark-*` badge
    /// right after the operator taps the button.
    @Published public private(set) var recentSparkByTimestamp: [String: String] = [:]

    private let api: CosmonAPIProtocol
    private var pollTask: Task<Void, Never>?
    private let pendingKey = "pending_notes"

    public init(api: CosmonAPIProtocol = CosmonAPIFactory.shared) {
        self.api = api
        loadPending()
    }

    deinit {
        pollTask?.cancel()
    }

    public var hasSession: Bool { state.sessionID != nil }

    public func start() async {
        isBusy = true
        defer { isBusy = false }
        do {
            _ = try await api.start()
            try? await flushPending()
            await refresh()
            connectivity = .connected
            lastError = nil
        } catch {
            handle(error)
        }
    }

    public func end() async {
        isBusy = true
        defer { isBusy = false }
        do {
            _ = try await api.end()
            await refresh()
            lastError = nil
        } catch {
            handle(error)
        }
    }

    public func send(text: String, tag: String?) async {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        isBusy = true
        defer { isBusy = false }
        do {
            try await api.note(trimmed, tag: tag)
            try? await flushPending()
            await refresh()
            connectivity = .connected
            lastError = nil
        } catch CosmonAPIError.notConnected {
            enqueue(text: trimmed, tag: tag)
            connectivity = .offline
            lastError = CosmonAPIError.notConnected.errorDescription
        } catch {
            handle(error)
        }
    }

    public func refresh() async {
        do {
            state = try await api.current()
            if connectivity != .connected {
                connectivity = .connected
            }
            try? await flushPending()
        } catch CosmonAPIError.notConnected {
            connectivity = .offline
        } catch {
            connectivity = .degraded
        }
    }

    /// Promote one note from the current session into a spark molecule
    /// via cs-api. Returns the newly created spark id on success, or
    /// an empty string when the server could not parse it out of the
    /// response (the promotion may still have succeeded — the mac side
    /// writes its sidecar unconditionally).
    ///
    /// On `notImplemented` (cs-api has not shipped the route yet), the
    /// error is surfaced through `lastError` and the UI keeps the
    /// button enabled — the operator can fall back to SSH-into-Mac
    /// and run `cs session promote <ts>` by hand.
    @discardableResult
    public func promote(note: Note) async -> String {
        guard let sid = state.sessionID?.value, !sid.isEmpty else {
            lastError = CosmonAPIError.noSessionOpen.errorDescription
            return ""
        }
        isBusy = true
        defer { isBusy = false }
        do {
            let sparkID = try await api.promoteNote(sessionID: sid, noteTimestamp: note.ts)
            promotedTimestamps.insert(note.ts)
            if !sparkID.isEmpty {
                recentSparkByTimestamp[note.ts] = sparkID
            }
            lastError = nil
            return sparkID
        } catch {
            handle(error)
            return ""
        }
    }

    public func startPolling(interval: TimeInterval) {
        pollTask?.cancel()
        pollTask = Task { [weak self] in
            let nanos = UInt64(max(1, interval) * 1_000_000_000)
            while !Task.isCancelled {
                await self?.refresh()
                try? await Task.sleep(nanoseconds: nanos)
            }
        }
    }

    public func stopPolling() {
        pollTask?.cancel()
        pollTask = nil
    }

    // MARK: - Offline queue

    private func enqueue(text: String, tag: String?) {
        pending.append(PendingNote(text: text, tag: tag))
        persistPending()
    }

    private func flushPending() async throws {
        guard !pending.isEmpty else { return }
        var remaining: [PendingNote] = []
        for item in pending {
            do {
                try await api.note(item.text, tag: item.tag)
            } catch {
                remaining.append(item)
            }
        }
        pending = remaining
        persistPending()
    }

    private func loadPending() {
        guard let data = UserDefaults.standard.data(forKey: pendingKey) else { return }
        pending = (try? JSONDecoder().decode([PendingNote].self, from: data)) ?? []
    }

    private func persistPending() {
        if let data = try? JSONEncoder().encode(pending) {
            UserDefaults.standard.set(data, forKey: pendingKey)
        }
    }

    private func handle(_ error: Error) {
        if let apiError = error as? CosmonAPIError {
            lastError = apiError.errorDescription
            switch apiError {
            case .notConnected:
                connectivity = .offline
            case .decodingFailed, .serverError, .invalidURL:
                connectivity = .degraded
            default:
                connectivity = .connected
            }
        } else {
            lastError = error.localizedDescription
            connectivity = .degraded
        }
    }
}
