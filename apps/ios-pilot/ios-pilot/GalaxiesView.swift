import SwiftUI

/// Store for the Galaxies pane — polls `GET /galaxies`.
@MainActor
public final class GalaxiesStore: ObservableObject {
    @Published public private(set) var galaxies: [Galaxy] = []
    @Published public private(set) var lastError: String?

    private let api: CosmonAPIProtocol
    private var pollTask: Task<Void, Never>?

    public init(api: CosmonAPIProtocol = CosmonAPIFactory.shared) {
        self.api = api
    }

    deinit { pollTask?.cancel() }

    public func refresh() async {
        do {
            let fresh = try await api.listGalaxies()
            galaxies = fresh
            lastError = nil
        } catch {
            lastError = (error as? CosmonAPIError)?.errorDescription ?? error.localizedDescription
        }
    }

    public func startPolling(interval: TimeInterval) {
        pollTask?.cancel()
        pollTask = Task { [weak self] in
            // Galaxies list changes slowly — cap the rate to at least 10s
            // so short polling intervals don't hammer the endpoint.
            let effective = max(interval, 10)
            let nanos = UInt64(effective * 1_000_000_000)
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
}

/// Read-only galaxy listing. v1 does **not** support switching the active
/// galaxy from iOS — cs-api still reads from the Mac's `$HOME/galaxies/cosmon/`.
struct GalaxiesView: View {
    @EnvironmentObject var settings: SettingsStore
    @StateObject private var store: GalaxiesStore

    init(store: GalaxiesStore? = nil) {
        _store = StateObject(wrappedValue: store ?? GalaxiesStore())
    }

    var body: some View {
        NavigationStack {
            Group {
                if !isURLValid {
                    emptyConnect
                } else if store.galaxies.isEmpty {
                    emptyState
                } else {
                    List(store.galaxies) { g in
                        row(for: g)
                    }
                    .listStyle(.insetGrouped)
                    .refreshable { await store.refresh() }
                }
            }
            .navigationTitle("Galaxies")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button {
                        Task { await store.refresh() }
                    } label: {
                        Image(systemName: "arrow.clockwise")
                    }
                    .accessibilityLabel(Text("Rafraîchir"))
                }
            }
        }
        .task {
            await store.refresh()
            if settings.pollingEnabled {
                store.startPolling(interval: settings.pollingInterval)
            }
        }
        .onChange(of: settings.pollingEnabled) { _, enabled in
            enabled
                ? store.startPolling(interval: settings.pollingInterval)
                : store.stopPolling()
        }
        .onChange(of: settings.pollingInterval) { _, newValue in
            if settings.pollingEnabled {
                store.startPolling(interval: newValue)
            }
        }
    }

    private func row(for galaxy: Galaxy) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Text(galaxy.name)
                    .font(.headline)
                Spacer()
                if galaxy.runningCount > 0 {
                    Label("\(galaxy.runningCount)", systemImage: "circle.fill")
                        .labelStyle(.titleAndIcon)
                        .font(.caption)
                        .foregroundStyle(.green)
                }
            }
            HStack(spacing: 10) {
                Text("\(galaxy.pendingCount) pending")
                    .font(.caption)
                    .foregroundStyle(galaxy.pendingCount > 0 ? .orange : .secondary)
                if let la = galaxy.lastActivity, !la.isEmpty {
                    Text("• \(la)")
                        .font(.caption2.monospaced())
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
            }
            Text(galaxy.path)
                .font(.caption2.monospaced())
                .foregroundStyle(.secondary)
                .lineLimit(1)
                .truncationMode(.middle)
        }
        .padding(.vertical, 2)
    }

    private var emptyState: some View {
        VStack(spacing: 10) {
            Image(systemName: "circles.hexagongrid")
                .font(.system(size: 42))
                .foregroundStyle(.secondary)
            Text("Aucune galaxy détectée.")
                .font(.headline)
            Text("cs-api scanne le dossier passé via --galaxies-root (default $HOME/galaxies) côté Mac.")
                .font(.footnote)
                .multilineTextAlignment(.center)
                .foregroundStyle(.secondary)
            if let err = store.lastError {
                Text(err).font(.caption).foregroundStyle(.red)
            }
        }
        .padding()
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private var emptyConnect: some View {
        VStack(spacing: 10) {
            Image(systemName: "antenna.radiowaves.left.and.right.slash")
                .font(.system(size: 42))
                .foregroundStyle(.secondary)
            Text("Connecte cs-api dans Réglages.")
                .font(.headline)
        }
        .padding()
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private var isURLValid: Bool {
        URL(string: settings.apiURL) != nil && !settings.apiURL.isEmpty
    }
}

#Preview {
    GalaxiesView(store: GalaxiesStore(api: MockCosmonAPI()))
        .environmentObject(SettingsStore())
}
