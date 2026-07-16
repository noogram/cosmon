import SwiftUI
#if canImport(UIKit)
import UIKit
#endif

/// Store for the Whispers pane — polls `GET /whispers` and mediates
/// archive/spark actions.
@MainActor
public final class WhispersStore: ObservableObject {
    @Published public private(set) var whispers: [Whisper] = []
    @Published public private(set) var isBusy: Bool = false
    @Published public private(set) var lastError: String?
    @Published public private(set) var feedback: String?

    private let api: CosmonAPIProtocol
    private var pollTask: Task<Void, Never>?

    public init(api: CosmonAPIProtocol = CosmonAPIFactory.shared) {
        self.api = api
    }

    deinit { pollTask?.cancel() }

    public var unreadCount: Int { whispers.count }

    public func refresh() async {
        do {
            let fresh = try await api.listWhispers(limit: 50)
            whispers = fresh
            lastError = nil
        } catch let CosmonAPIError.serverError(msg) {
            lastError = "cs-api: \(msg)"
        } catch {
            lastError = (error as? CosmonAPIError)?.errorDescription ?? error.localizedDescription
        }
    }

    public func archive(_ whisper: Whisper) async -> Bool {
        isBusy = true
        defer { isBusy = false }
        do {
            try await api.archiveWhisper(id: whisper.wid)
            feedback = "Whisper archivé."
            await refresh()
            return true
        } catch {
            lastError = (error as? CosmonAPIError)?.errorDescription ?? error.localizedDescription
            return false
        }
    }

    public func spark(_ whisper: Whisper) async -> Bool {
        isBusy = true
        defer { isBusy = false }
        do {
            let newID = try await api.sparkWhisper(id: whisper.wid, text: nil, nucleon: nil)
            feedback = newID.isEmpty ? "Spark créé." : "Spark \(newID) créé."
            await refresh()
            return true
        } catch {
            lastError = (error as? CosmonAPIError)?.errorDescription ?? error.localizedDescription
            return false
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
}

/// Whispers tab. On iPhone → navigation stack. On iPad → split view
/// (list on left, detail on right) via `NavigationSplitView`.
struct WhispersView: View {
    @EnvironmentObject var settings: SettingsStore
    @StateObject private var store: WhispersStore

    @State private var selected: Whisper?

    init(store: WhispersStore? = nil) {
        _store = StateObject(wrappedValue: store ?? WhispersStore())
    }

    var body: some View {
        NavigationSplitView {
            list
                .navigationTitle("Whispers")
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
        } detail: {
            if let w = selected {
                WhisperDetailView(whisper: w, store: store) {
                    // After a destructive action (archive/spark) the item disappears.
                    selected = nil
                }
            } else {
                emptyDetail
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

    @ViewBuilder
    private var list: some View {
        if !isURLValid {
            emptyConnect
        } else if store.whispers.isEmpty {
            emptyList
        } else {
            List(selection: $selected) {
                ForEach(store.whispers) { w in
                    NavigationLink(value: w) {
                        row(for: w)
                    }
                    .tag(w)
                }
            }
            .listStyle(.insetGrouped)
            .refreshable { await store.refresh() }
            .navigationDestination(for: Whisper.self) { w in
                WhisperDetailView(whisper: w, store: store) {
                    selected = nil
                }
            }
        }
    }

    private func row(for w: Whisper) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 6) {
                Text(w.senderNucleonID ?? "?")
                    .font(.caption.monospaced())
                    .foregroundStyle(Color.accentColor)
                Spacer()
                Text(Self.relativeTime(from: w.receivedAtDate))
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }
            let truncated = MarkdownView.truncatedMarkdown(w.preview, maxChars: 180)
            MarkdownView(text: truncated, theme: .compact)
                .lineLimit(2)
        }
        .padding(.vertical, 2)
    }

    private var emptyList: some View {
        VStack(spacing: 12) {
            Image(systemName: "tray")
                .font(.system(size: 48))
                .foregroundStyle(.secondary)
            Text("Aucun whisper.")
                .font(.headline)
            Text("Envoie un message dans `#cosmon-whispers` depuis Element pour tester.")
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
            Text("L'URL Tailscale doit pointer vers ton Mac (ex. http://100.64.0.12:4222).")
                .font(.footnote)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
        }
        .padding()
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private var emptyDetail: some View {
        VStack(spacing: 8) {
            Image(systemName: "bubble.left.and.bubble.right")
                .font(.system(size: 46))
                .foregroundStyle(.secondary)
            Text("Sélectionne un whisper")
                .font(.headline)
            Text("Le corps et les actions apparaissent ici.")
                .font(.footnote)
                .foregroundStyle(.secondary)
        }
        .padding()
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private var isURLValid: Bool {
        URL(string: settings.apiURL) != nil && !settings.apiURL.isEmpty
    }

    static func relativeTime(from date: Date) -> String {
        let seconds = Int(Date().timeIntervalSince(date))
        if seconds < 60 { return "à l'instant" }
        if seconds < 3600 { return "il y a \(seconds / 60)m" }
        if seconds < 86_400 { return "il y a \(seconds / 3600)h" }
        return "il y a \(seconds / 86_400)j"
    }
}

struct WhisperDetailView: View {
    @EnvironmentObject var settings: SettingsStore
    let whisper: Whisper
    @ObservedObject var store: WhispersStore
    let onDismiss: () -> Void

    @State private var showSparkConfirm = false
    @State private var showArchiveConfirm = false

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                header
                Divider()
                body_
                Divider()
                actions
            }
            .padding()
        }
        .navigationTitle("Whisper")
        .navigationBarTitleDisplayMode(.inline)
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(whisper.senderNucleonID ?? "?")
                .font(.headline)
            if let mx = whisper.senderMxID {
                Text(mx)
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
            }
            Text(whisper.receivedAt)
                .font(.caption2.monospaced())
                .foregroundStyle(.secondary)
        }
    }

    private var body_: some View {
        VStack(alignment: .leading, spacing: 12) {
            if whisper.body.isEmpty {
                Text("(corps vide)")
                    .font(.body)
                    .foregroundStyle(.secondary)
            } else {
                MarkdownView(text: whisper.body, theme: settings.markdownTheme.theme)
                    .textSelection(.enabled)
            }

            VStack(alignment: .leading, spacing: 4) {
                Text("Metadata")
                    .font(.caption.bold())
                    .foregroundStyle(.secondary)
                metadataRow("id", whisper.wid)
                metadataRow("room", whisper.roomID)
                if let path = whisper.path {
                    metadataRow("path", path)
                }
            }
        }
    }

    private func metadataRow(_ key: String, _ value: String) -> some View {
        HStack(alignment: .top, spacing: 8) {
            Text(key)
                .font(.caption2.monospaced())
                .foregroundStyle(.secondary)
                .frame(width: 40, alignment: .leading)
            Text(value)
                .font(.caption2.monospaced())
                .lineLimit(2)
                .textSelection(.enabled)
        }
    }

    private var actions: some View {
        VStack(spacing: 8) {
            if let fb = store.feedback {
                Text(fb)
                    .font(.footnote)
                    .foregroundStyle(.green)
            }
            if let err = store.lastError {
                Text(err)
                    .font(.footnote)
                    .foregroundStyle(.red)
            }
            HStack(spacing: 12) {
                Button(role: .destructive) {
                    showArchiveConfirm = true
                } label: {
                    Label("Archiver", systemImage: "archivebox")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.bordered)
                .disabled(store.isBusy)

                Button {
                    showSparkConfirm = true
                } label: {
                    Label("Transformer en spark", systemImage: "sparkles")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .disabled(store.isBusy)
            }
        }
        .confirmationDialog(
            "Archiver ce whisper ?",
            isPresented: $showArchiveConfirm,
            titleVisibility: .visible
        ) {
            Button("Archiver", role: .destructive) {
                Task {
                    if await store.archive(whisper) { onDismiss() }
                }
            }
            Button("Annuler", role: .cancel) {}
        }
        .confirmationDialog(
            "Transformer en spark ?",
            isPresented: $showSparkConfirm,
            titleVisibility: .visible
        ) {
            Button("Créer spark") {
                Task {
                    if await store.spark(whisper) { onDismiss() }
                }
            }
            Button("Annuler", role: .cancel) {}
        } message: {
            Text("Crée une molécule `idea` avec le corps du whisper.")
        }
    }
}

#Preview {
    WhispersView(store: WhispersStore(api: MockCosmonAPI()))
        .environmentObject(SettingsStore())
}
