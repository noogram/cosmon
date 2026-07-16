import SwiftUI

/// Tag filter exposed in the Inbox top-bar picker. Mirrors the
/// `mac-pilot` `InboxFilter` (see `apps/mac-pilot/mac-pilot/InboxView.swift`)
/// so the Mac and iOS surfaces read as the same object at a glance —
/// wheat-paste compliance (§8k' of the delib-becf synthesis).
public enum InboxTagFilter: String, CaseIterable, Identifiable {
    case all  = "Tous"
    case hot  = "temp:hot"
    case warm = "temp:warm"

    public var id: String { rawValue }

    /// The tag string applied client-side. `nil` → no filter.
    public var tag: String? {
        switch self {
        case .all:  return nil
        case .hot:  return "temp:hot"
        case .warm: return "temp:warm"
        }
    }
}

/// Store for the Inbox pane — polls `GET /inbox`.
@MainActor
public final class InboxStore: ObservableObject {
    @Published public private(set) var items: [MoleculeSummary] = []
    @Published public private(set) var lastError: String?
    @Published public private(set) var feedback: String?
    @Published public private(set) var isLoading: Bool = false
    @Published public private(set) var isBusy: Bool = false

    /// Status filter forwarded as `?status=<csv>`. Defaults to pending/running.
    @Published public var statusFilter: String = "pending,running"

    /// Tag filter applied client-side after `GET /inbox`. Bound to the
    /// segmented picker in `InboxView`.
    @Published public var tagFilter: InboxTagFilter = .all

    /// When true, only items carrying `temp:hot` are returned.
    /// Preserved for backward compat with `SettingsStore.onlyHot` but
    /// `tagFilter` takes precedence when not `.all`.
    @Published public var onlyHot: Bool = false

    private let api: CosmonAPIProtocol
    private var pollTask: Task<Void, Never>?
    private var allItems: [MoleculeSummary] = []

    public init(api: CosmonAPIProtocol = CosmonAPIFactory.shared) {
        self.api = api
    }

    deinit { pollTask?.cancel() }

    public var hotCount: Int {
        allItems.filter(\.isHot).count
    }

    public func refresh() async {
        isLoading = true
        defer { isLoading = false }
        do {
            let fresh = try await api.listInbox(status: statusFilter, limit: nil)
            allItems = fresh
            items = applyFilter(fresh)
            lastError = nil
        } catch {
            lastError = (error as? CosmonAPIError)?.errorDescription ?? error.localizedDescription
        }
    }

    public func reapplyFilter() {
        items = applyFilter(allItems)
    }

    private func applyFilter(_ all: [MoleculeSummary]) -> [MoleculeSummary] {
        if let tag = tagFilter.tag {
            return all.filter { $0.tags.contains(tag) }
        }
        if onlyHot {
            return all.filter(\.isHot)
        }
        return all
    }

    /// Kick off `POST /molecules/{id}/tackle`. Refreshes the list on
    /// success so the new `running` status and worker are reflected.
    public func tackle(_ molecule: MoleculeSummary) async -> Bool {
        isBusy = true
        defer { isBusy = false }
        do {
            try await api.tackleMolecule(id: molecule.id)
            feedback = "Worker lancé sur le Mac."
            await refresh()
            return true
        } catch {
            lastError = (error as? CosmonAPIError)?.errorDescription ?? error.localizedDescription
            return false
        }
    }

    /// Kick off `POST /molecules/{id}/tag`. Used by the detail pane to
    /// promote `temp:warm` → `temp:hot` (and the reverse) directly from
    /// the iPhone/iPad.
    public func tag(_ molecule: MoleculeSummary, add: [String], remove: [String]) async -> Bool {
        isBusy = true
        defer { isBusy = false }
        do {
            try await api.tagMolecule(id: molecule.id, add: add, remove: remove)
            feedback = tagFeedback(add: add, remove: remove)
            await refresh()
            return true
        } catch {
            lastError = (error as? CosmonAPIError)?.errorDescription ?? error.localizedDescription
            return false
        }
    }

    private func tagFeedback(add: [String], remove: [String]) -> String {
        switch (add.isEmpty, remove.isEmpty) {
        case (false, true):  return "Ajouté: \(add.joined(separator: ", "))"
        case (true, false):  return "Retiré: \(remove.joined(separator: ", "))"
        case (false, false): return "Tags mis à jour"
        default:             return "Aucun changement"
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

struct InboxView: View {
    @EnvironmentObject var settings: SettingsStore
    @StateObject private var store: InboxStore

    @State private var selected: MoleculeSummary?

    init(store: InboxStore? = nil) {
        _store = StateObject(wrappedValue: store ?? InboxStore())
    }

    var body: some View {
        NavigationSplitView {
            VStack(spacing: 0) {
                filterPicker
                    .padding(.horizontal)
                    .padding(.top, 8)
                    .padding(.bottom, 4)
                list
            }
                .navigationTitle("Inbox")
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
            if let m = selected {
                MoleculeDetailView(molecule: m, store: store) { selected = nil }
            } else {
                emptyDetail
            }
        }
        .task {
            store.onlyHot = settings.onlyHot
            await store.refresh()
            if settings.pollingEnabled {
                store.startPolling(interval: settings.pollingInterval)
            }
        }
        .onChange(of: settings.onlyHot) { _, value in
            store.onlyHot = value
            store.reapplyFilter()
        }
        .onChange(of: store.tagFilter) { _, _ in
            store.reapplyFilter()
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

    private var filterPicker: some View {
        Picker("Filtre", selection: $store.tagFilter) {
            ForEach(InboxTagFilter.allCases) { f in
                Text(f.rawValue).tag(f)
            }
        }
        .pickerStyle(.segmented)
        .labelsHidden()
    }

    @ViewBuilder
    private var list: some View {
        if !isURLValid {
            emptyConnect
        } else if store.items.isEmpty {
            emptyList
        } else {
            List(selection: $selected) {
                ForEach(store.items) { m in
                    NavigationLink(value: m) {
                        row(for: m)
                    }
                    .tag(m)
                }
            }
            .listStyle(.insetGrouped)
            .refreshable { await store.refresh() }
            .navigationDestination(for: MoleculeSummary.self) { m in
                MoleculeDetailView(molecule: m, store: store) { selected = nil }
            }
        }
    }

    private func row(for m: MoleculeSummary) -> some View {
        HStack(spacing: 10) {
            Text(m.kindEmoji)
                .font(.title3)
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Text(m.shortID)
                        .font(.caption.monospaced())
                        .foregroundStyle(Color.accentColor)
                    if m.isHot {
                        Text("🔥").font(.caption2)
                    }
                    statusBadge(m.status)
                }
                topicLine(for: m)
                if !m.tags.isEmpty {
                    Text(m.tags.joined(separator: " · "))
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
            }
            Spacer()
        }
        .padding(.vertical, 2)
    }

    /// Inbox row: render the topic as markdown so **gras**, `code`, and
    /// [liens] show up stylées. Long topics are truncated to ~72 chars
    /// on a safe markdown boundary to keep rows single-line.
    @ViewBuilder
    private func topicLine(for m: MoleculeSummary) -> some View {
        let raw = (m.topic?.isEmpty == false) ? m.topic! : m.formula
        let preview = MarkdownView.truncatedMarkdown(raw, maxChars: 72)
        MarkdownView(text: preview, theme: .compact)
            .lineLimit(1)
    }

    private func statusBadge(_ status: String) -> some View {
        Text(status)
            .font(.caption2)
            .padding(.horizontal, 5)
            .padding(.vertical, 1)
            .background(Self.statusColor(status).opacity(0.18))
            .foregroundStyle(Self.statusColor(status))
            .cornerRadius(3)
    }

    static func statusColor(_ status: String) -> Color {
        switch status {
        case "running", "active": return .green
        case "queued":            return .orange
        case "pending":           return .secondary
        default:                  return .secondary
        }
    }

    private var emptyList: some View {
        VStack(spacing: 12) {
            Image(systemName: "tray")
                .font(.system(size: 48))
                .foregroundStyle(.secondary)
            Text(settings.onlyHot ? "Aucune molecule temp:hot." : "Aucune molecule pending.")
                .font(.headline)
            Text(settings.onlyHot
                 ? "Décoche 'Only temp:hot' dans Réglages pour voir toutes les molécules pending/running."
                 : "Pas de molécule à tackler. Nucléate-en une depuis le terminal sur le Mac.")
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

    private var emptyDetail: some View {
        VStack(spacing: 8) {
            Image(systemName: "doc.text.magnifyingglass")
                .font(.system(size: 46))
                .foregroundStyle(.secondary)
            Text("Sélectionne une molécule")
                .font(.headline)
            Text("Le topic et les tags apparaissent ici.")
                .font(.footnote)
                .foregroundStyle(.secondary)
        }
        .padding()
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private var isURLValid: Bool {
        URL(string: settings.apiURL) != nil && !settings.apiURL.isEmpty
    }
}

struct MoleculeDetailView: View {
    @EnvironmentObject var settings: SettingsStore
    let molecule: MoleculeSummary
    @ObservedObject var store: InboxStore
    let onDismiss: () -> Void

    @State private var showTackleConfirm = false

    init(molecule: MoleculeSummary,
         store: InboxStore,
         onDismiss: @escaping () -> Void = {}) {
        self.molecule = molecule
        self.store = store
        self.onDismiss = onDismiss
    }

    /// Latest-known state of this molecule — after a tackle/tag we want
    /// to show the new status/tags without waiting for the user to pop
    /// and re-push the detail view.
    private var live: MoleculeSummary {
        store.items.first(where: { $0.id == molecule.id }) ?? molecule
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                header
                Divider()
                topicBlock
                if !live.tags.isEmpty {
                    tagsBlock
                }
                Divider()
                metadataBlock
                Divider()
                actionsBlock
            }
            .padding()
        }
        .navigationTitle(live.shortID)
        .navigationBarTitleDisplayMode(.inline)
    }

    private var header: some View {
        HStack(spacing: 10) {
            Text(live.kindEmoji).font(.largeTitle)
            VStack(alignment: .leading, spacing: 4) {
                Text(live.id)
                    .font(.footnote.monospaced())
                    .foregroundStyle(Color.accentColor)
                    .textSelection(.enabled)
                HStack(spacing: 6) {
                    Text(live.status.capitalized)
                        .font(.caption)
                        .padding(.horizontal, 6)
                        .padding(.vertical, 2)
                        .background(InboxView.statusColor(live.status).opacity(0.15))
                        .foregroundStyle(InboxView.statusColor(live.status))
                        .cornerRadius(4)
                    Text(live.formula)
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                }
            }
            Spacer()
        }
    }

    private var topicBlock: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text("Topic").font(.caption.bold()).foregroundStyle(.secondary)
            if let raw = live.topic, !raw.isEmpty {
                MarkdownView(text: raw, theme: settings.markdownTheme.theme)
                    .textSelection(.enabled)
            } else {
                Text("(aucun topic)")
                    .font(.body)
                    .foregroundStyle(.secondary)
            }
        }
    }

    private var tagsBlock: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text("Tags").font(.caption.bold()).foregroundStyle(.secondary)
            FlowTagsView(tags: live.tags)
        }
    }

    private var metadataBlock: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Metadata").font(.caption.bold()).foregroundStyle(.secondary)
            metadataRow("créé", live.createdAt)
            metadataRow("modifié", live.updatedAt)
            if let w = live.assignedWorker {
                metadataRow("worker", w)
            }
        }
    }

    private func metadataRow(_ key: String, _ value: String) -> some View {
        HStack(alignment: .top, spacing: 8) {
            Text(key)
                .font(.caption2.monospaced())
                .foregroundStyle(.secondary)
                .frame(width: 70, alignment: .leading)
            Text(value)
                .font(.caption2.monospaced())
                .textSelection(.enabled)
                .lineLimit(2)
        }
    }

    /// Actions surfaced on iOS — only the **non-destructive** verbs
    /// from Feynman's split (delib-20260423-becf synthesis §I). `done`
    /// and `collapse` remain Mac-only because a 6" screen is not a
    /// place to kill a molecule by accident.
    private var actionsBlock: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Actions").font(.caption.bold()).foregroundStyle(.secondary)
            if let fb = store.feedback {
                Text(fb).font(.footnote).foregroundStyle(.green)
            }
            if let err = store.lastError {
                Text(err).font(.footnote).foregroundStyle(.red)
            }
            tackleButton
            tagButtons
            Text("Les verbes destructifs (done, collapse) restent sur Mac.")
                .font(.caption2)
                .foregroundStyle(.secondary)
        }
    }

    private var tackleButton: some View {
        Button {
            showTackleConfirm = true
        } label: {
            Label("Tackle", systemImage: "play.fill")
                .frame(maxWidth: .infinity)
        }
        .buttonStyle(.borderedProminent)
        .disabled(store.isBusy || live.status == "running" || live.status == "active")
        .confirmationDialog(
            "Tackle \(live.shortID) ?",
            isPresented: $showTackleConfirm,
            titleVisibility: .visible
        ) {
            Button("Lancer le worker") {
                Task {
                    if await store.tackle(live) { onDismiss() }
                }
            }
            Button("Annuler", role: .cancel) {}
        } message: {
            Text("Le worker spawnera sur le Mac (via cs-api).")
        }
    }

    private var tagButtons: some View {
        HStack(spacing: 12) {
            if live.tags.contains("temp:hot") {
                Button {
                    Task { _ = await store.tag(live, add: ["temp:warm"], remove: ["temp:hot"]) }
                } label: {
                    Label("Refroidir", systemImage: "thermometer.low")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.bordered)
                .disabled(store.isBusy)
            } else {
                Button {
                    Task { _ = await store.tag(live, add: ["temp:hot"], remove: ["temp:warm"]) }
                } label: {
                    Label("temp:hot", systemImage: "flame")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.bordered)
                .disabled(store.isBusy)
            }
            if !live.tags.contains("temp:warm") && !live.tags.contains("temp:hot") {
                Button {
                    Task { _ = await store.tag(live, add: ["temp:warm"], remove: []) }
                } label: {
                    Label("temp:warm", systemImage: "thermometer.medium")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.bordered)
                .disabled(store.isBusy)
            }
        }
    }
}

/// Simple flow-layout of tag chips — wraps onto multiple lines.
struct FlowTagsView: View {
    let tags: [String]

    var body: some View {
        let chunks = tags.chunked(into: 3)
        VStack(alignment: .leading, spacing: 4) {
            ForEach(Array(chunks.enumerated()), id: \.offset) { _, row in
                HStack(spacing: 4) {
                    ForEach(row, id: \.self) { tag in
                        Text(tag)
                            .font(.caption2)
                            .padding(.horizontal, 6)
                            .padding(.vertical, 2)
                            .background(Color.accentColor.opacity(0.12))
                            .cornerRadius(4)
                    }
                    Spacer()
                }
            }
        }
    }
}

private extension Array {
    func chunked(into size: Int) -> [[Element]] {
        guard size > 0 else { return [self] }
        return stride(from: 0, to: count, by: size).map {
            Array(self[$0..<Swift.min($0 + size, count)])
        }
    }
}

#Preview("Inbox") {
    InboxView(store: InboxStore(api: MockCosmonAPI()))
        .environmentObject(SettingsStore())
}

#Preview("Detail — pending") {
    NavigationStack {
        MoleculeDetailView(
            molecule: MoleculeSummary(
                id: "task-20260423-preview",
                kind: "task",
                status: "pending",
                topic: "Preview molecule — tackle-ready",
                tags: ["temp:hot"],
                createdAt: "2026-04-23T10:00:00Z",
                updatedAt: "2026-04-23T11:00:00Z",
                formula: "task-work",
                assignedWorker: nil
            ),
            store: InboxStore(api: MockCosmonAPI())
        )
    }
    .environmentObject(SettingsStore())
}
