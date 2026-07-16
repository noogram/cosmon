//
//  InboxView.swift
//  mac-pilot
//
//  Lists pending / running molecules by shelling out to `cs observe --json`.
//  Refresh is rate-limited to one call per 10 s — the shell-out is expensive
//  and the contents change slowly. Clicking a row shells out `cs observe <id>
//  --json` to fetch the full detail (topic, tags, molecule_dir), then offers
//  tackle / open worktree / collapse actions.
//

import SwiftUI
import AppKit

/// Simple tag filter — top row of the Inbox pane.
enum InboxFilter: String, CaseIterable, Identifiable {
    case all      = "Tous"
    case hot      = "temp:hot"
    case warm     = "temp:warm"

    var id: String { rawValue }

    var tagArg: String? {
        switch self {
        case .all:  return nil
        case .hot:  return "temp:hot"
        case .warm: return "temp:warm"
        }
    }
}

@MainActor
final class InboxViewModel: ObservableObject {

    enum Feedback: Equatable {
        case idle
        case busy(String)
        case success(String)
        case failure(String)
    }

    @Published private(set) var items: [MoleculeSummary] = []
    @Published var filter: InboxFilter = .all {
        didSet {
            if filter != oldValue {
                lastFetch = .distantPast
                Task { await refresh(force: true) }
            }
        }
    }
    @Published var selectedID: String?
    @Published private(set) var detail: MoleculeDetail?
    @Published private(set) var feedback: Feedback = .idle

    /// Count of items currently visible with `temp:hot` — drives the tab badge.
    @Published private(set) var hotCount: Int = 0

    private var lastFetch: Date = .distantPast
    private let minRefreshInterval: TimeInterval = 10
    private var timer: Timer?

    func startPolling(every seconds: TimeInterval = 15) {
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

    func refresh(force: Bool = false) async {
        if !force && Date().timeIntervalSince(lastFetch) < minRefreshInterval {
            return
        }
        do {
            let fresh = try await CosmonBridge.listInbox(tag: filter.tagArg)
            if fresh.map(\.id) != items.map(\.id) || fresh.map(\.status) != items.map(\.status) {
                items = fresh
            }
            lastFetch = Date()
            // Background hot count (no filter).
            if filter == .hot {
                hotCount = fresh.count
            } else {
                let hot = (try? await CosmonBridge.listInbox(tag: "temp:hot")) ?? []
                hotCount = hot.count
            }
        } catch {
            feedback = .failure(errorMessage(error))
            scheduleFeedbackClear(after: 3.0)
        }
    }

    func select(_ id: String) {
        selectedID = id
        detail = nil
        Task {
            do {
                detail = try await CosmonBridge.moleculeDetail(id: id)
            } catch {
                feedback = .failure(errorMessage(error))
                scheduleFeedbackClear(after: 3.0)
            }
        }
    }

    func clearSelection() {
        selectedID = nil
        detail = nil
    }

    func tackle(_ id: String) async {
        feedback = .busy("Tackle \(id)…")
        do {
            try await CosmonBridge.tackle(moleculeID: id)
            feedback = .success("Worker lancé")
            clearSelection()
            lastFetch = .distantPast
            await refresh(force: true)
            scheduleFeedbackClear(after: 1.5)
        } catch {
            feedback = .failure(errorMessage(error))
            scheduleFeedbackClear(after: 3.0)
        }
    }

    func whisper(_ id: String, body: String) async {
        let trimmed = body.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        feedback = .busy("Whisper \(id)…")
        do {
            try await CosmonBridge.whisper(moleculeID: id, body: trimmed)
            feedback = .success("Whisper envoyé")
            scheduleFeedbackClear(after: 1.5)
        } catch {
            feedback = .failure(errorMessage(error))
            scheduleFeedbackClear(after: 4.0)
        }
    }

    func collapse(_ id: String, reason: String) async {
        feedback = .busy("Collapse \(id)…")
        do {
            try await CosmonBridge.collapse(moleculeID: id, reason: reason)
            feedback = .success("Molecule collapsée")
            clearSelection()
            lastFetch = .distantPast
            await refresh(force: true)
            scheduleFeedbackClear(after: 1.5)
        } catch {
            feedback = .failure(errorMessage(error))
            scheduleFeedbackClear(after: 3.0)
        }
    }

    func openWorktree() {
        guard let d = detail, let dir = d.moleculeDir else { return }
        // molecule_dir points at the state dir; the worktree lives under
        // <galaxy>/.worktrees/<mol_id>/ and may not exist for pending.
        let worktree = CosmonBridge.galaxyRoot
            .appendingPathComponent(".worktrees/\(d.id)", isDirectory: true)
        if FileManager.default.fileExists(atPath: worktree.path) {
            CosmonBridge.openInFinder(path: worktree)
        } else {
            CosmonBridge.openInFinder(path: URL(fileURLWithPath: dir))
        }
    }

    // MARK: - Helpers

    private func scheduleFeedbackClear(after seconds: TimeInterval) {
        Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: UInt64(seconds * 1_000_000_000))
            if case .busy = self?.feedback { return }
            self?.feedback = .idle
        }
    }

    private func errorMessage(_ error: Error) -> String {
        if let cosmon = error as? CosmonError {
            return cosmon.errorDescription ?? "Erreur inconnue"
        }
        return error.localizedDescription
    }
}

struct InboxView: View {
    @ObservedObject var model: InboxViewModel
    @State private var collapsePrompt = false
    @State private var collapseReason = ""
    @State private var whisperPrompt = false
    @State private var whisperBody = ""
    @AppStorage("markdown_theme") private var markdownThemeRaw: String = MarkdownThemeID.relaxed.rawValue

    /// The operator's current markdown theme choice, read from
    /// `@AppStorage`. Defaults to `.relaxed` when the key is missing.
    private var markdownTheme: MarkdownTheme {
        (MarkdownThemeID(rawValue: markdownThemeRaw) ?? .relaxed).theme
    }

    var body: some View {
        Group {
            if let detail = model.detail, model.selectedID != nil {
                detailPane(detail)
            } else if let id = model.selectedID, model.detail == nil {
                loadingDetail(id: id)
            } else {
                listPane
            }
        }
        .padding(.horizontal, 12)
        .padding(.top, 4)
    }

    private var listPane: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text("Inbox").font(.headline)
                Spacer()
                feedbackIndicator
            }
            Picker("", selection: $model.filter) {
                ForEach(InboxFilter.allCases) { f in
                    Text(f.rawValue).tag(f)
                }
            }
            .pickerStyle(.segmented)
            .labelsHidden()

            if model.items.isEmpty {
                Text("Aucune molecule pending.")
                    .font(.footnote)
                    .foregroundColor(.secondary)
                    .padding(.top, 8)
                Spacer()
            } else {
                ScrollView {
                    VStack(alignment: .leading, spacing: 3) {
                        ForEach(model.items) { item in
                            Button {
                                model.select(item.id)
                            } label: {
                                row(item)
                            }
                            .buttonStyle(.plain)
                            Divider()
                        }
                    }
                    .padding(.top, 2)
                }
            }
        }
    }

    private func row(_ item: MoleculeSummary) -> some View {
        HStack(spacing: 6) {
            Text(item.kindEmoji)
                .font(.body)
            Text(item.shortID)
                .font(.caption.monospaced())
                .foregroundColor(.accentColor)
            Text(item.formula)
                .font(.caption2)
                .foregroundColor(.secondary)
                .lineLimit(1)
            Spacer()
            statusBadge(item.status)
        }
        .padding(.vertical, 2)
        .contentShape(Rectangle())
    }

    private func statusBadge(_ status: String) -> some View {
        Text(status)
            .font(.caption2)
            .padding(.horizontal, 5)
            .padding(.vertical, 1)
            .background(statusColor(status).opacity(0.18))
            .foregroundColor(statusColor(status))
            .cornerRadius(3)
    }

    private func statusColor(_ status: String) -> Color {
        switch status {
        case "running", "active": return .green
        case "queued":            return .orange
        case "pending":           return .secondary
        default:                  return .secondary
        }
    }

    @ViewBuilder
    private var feedbackIndicator: some View {
        switch model.feedback {
        case .idle: EmptyView()
        case .busy(let s):
            HStack(spacing: 4) {
                ProgressView().controlSize(.small)
                Text(s).font(.caption2).foregroundColor(.secondary)
            }
        case .success(let s):
            Text(s).font(.caption2).foregroundColor(.green)
        case .failure(let s):
            Text(s).font(.caption2).foregroundColor(.red).lineLimit(1)
        }
    }

    private func loadingDetail(id: String) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Button {
                    model.clearSelection()
                } label: {
                    Label("Retour", systemImage: "chevron.left")
                }
                .buttonStyle(.plain)
                Spacer()
            }
            ProgressView("Chargement \(id)…")
            Spacer()
        }
    }

    /// DAG view of a molecule's typed links, grouped by relation kind.
    ///
    /// Groups are rendered in a fixed semantic order (blocked-by → blocks →
    /// refines → refined-by → decay lineage → peers). Molecule-id targets
    /// are rendered as buttons that switch the Inbox selection — one click,
    /// one navigation, no worktree round-trip. Free-form `Entangled`
    /// targets (URLs, previous kinds) render as plain secondary text.
    @ViewBuilder
    private func linksSection(_ links: [MoleculeTypedLink]) -> some View {
        let grouped = Dictionary(grouping: links, by: \.relation)
        let ordered = grouped.keys.sorted { $0.displayRank < $1.displayRank }
        VStack(alignment: .leading, spacing: 6) {
            Text("Liens")
                .font(.caption.bold())
                .foregroundColor(.secondary)
            ForEach(ordered, id: \.self) { relation in
                if let items = grouped[relation], !items.isEmpty {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(relation.header)
                            .font(.caption2)
                            .foregroundColor(.secondary)
                        ForEach(items) { link in
                            linkRow(link)
                        }
                    }
                }
            }
        }
    }

    @ViewBuilder
    private func linkRow(_ link: MoleculeTypedLink) -> some View {
        if link.targetIsMolecule {
            Button {
                model.select(link.target)
            } label: {
                HStack(spacing: 4) {
                    Text(moleculeKindEmoji(link.target))
                    Text(link.target)
                        .font(.caption.monospaced())
                        .foregroundColor(.accentColor)
                    Spacer()
                }
                .padding(.vertical, 1)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
        } else {
            Text(link.target)
                .font(.caption2.monospaced())
                .foregroundColor(.secondary)
                .lineLimit(1)
                .truncationMode(.middle)
        }
    }

    /// Derive the kind emoji from an id prefix — same mapping as
    /// `MoleculeSummary.kindEmoji` so a clicked link displays with the
    /// same glyph it had in the Inbox list. Kept local to avoid
    /// reaching for a full `MoleculeSummary` when we only have an id.
    private func moleculeKindEmoji(_ id: String) -> String {
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

    private func detailPane(_ d: MoleculeDetail) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Button {
                    model.clearSelection()
                } label: {
                    Label("Retour", systemImage: "chevron.left")
                }
                .buttonStyle(.plain)
                Spacer()
                feedbackIndicator
            }

            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Text(d.id)
                        .font(.caption.monospaced())
                        .foregroundColor(.accentColor)
                    Text(d.formula).font(.caption2).foregroundColor(.secondary)
                    Spacer()
                    Text(d.status).font(.caption2.bold())
                }
                if !d.tags.isEmpty {
                    HStack(spacing: 4) {
                        ForEach(d.tags, id: \.self) { t in
                            Text(t)
                                .font(.caption2)
                                .padding(.horizontal, 4)
                                .padding(.vertical, 1)
                                .background(Color.accentColor.opacity(0.12))
                                .cornerRadius(3)
                        }
                    }
                }
            }

            Divider()

            ScrollView {
                VStack(alignment: .leading, spacing: 4) {
                    Text("Topic").font(.caption.bold()).foregroundColor(.secondary)
                    if d.topic.isEmpty {
                        Text("(aucun topic)")
                            .font(.footnote)
                            .foregroundColor(.secondary)
                    } else {
                        MarkdownView(text: d.topic, theme: markdownTheme)
                            .textSelection(.enabled)
                    }
                    if let w = d.worker {
                        Text("Worker: \(w)")
                            .font(.caption2.monospaced())
                            .foregroundColor(.secondary)
                            .padding(.top, 4)
                    }
                    if !d.typedLinks.isEmpty {
                        Divider().padding(.vertical, 4)
                        linksSection(d.typedLinks)
                    }
                }
            }

            Divider()

            HStack(spacing: 6) {
                Button {
                    Task { await model.tackle(d.id) }
                } label: {
                    Label("Tackle", systemImage: "play.fill")
                }
                .disabled(d.status == "running")
                Button {
                    model.openWorktree()
                } label: {
                    Label("Worktree", systemImage: "folder")
                }
                Button {
                    whisperBody = ""
                    whisperPrompt = true
                } label: {
                    Label("Whisper", systemImage: "bubble.left.fill")
                }
                .disabled(!isWhisperable(d))
                Button(role: .destructive) {
                    collapseReason = ""
                    collapsePrompt = true
                } label: {
                    Label("Collapse", systemImage: "xmark.circle")
                }
                Spacer()
            }
            .padding(.bottom, 4)
        }
        .alert("Collapse \(d.id) ?", isPresented: $collapsePrompt) {
            TextField("raison", text: $collapseReason)
            Button("Annuler", role: .cancel) { }
            Button("Collapse", role: .destructive) {
                Task { await model.collapse(d.id, reason: collapseReason) }
            }
        } message: {
            Text("Irréversible. Indique la raison pour la trace.")
        }
        .sheet(isPresented: $whisperPrompt) {
            whisperSheet(d)
        }
    }

    /// Whispering only makes sense against a live worker: the `cs
    /// whisper` CLI refuses to inject into a molecule whose pane is not
    /// running an allowed command (see `allowed_commands` in
    /// `.cosmon/config.toml`). Locking the button to `running` / `active`
    /// keeps the UX honest.
    private func isWhisperable(_ d: MoleculeDetail) -> Bool {
        d.status == "running" || d.status == "active"
    }

    private func whisperSheet(_ d: MoleculeDetail) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Whisper → \(d.id)")
                .font(.headline)
            Text("Le texte est injecté dans le pane tmux du worker (≤8 KiB).")
                .font(.caption)
                .foregroundColor(.secondary)
            TextEditor(text: $whisperBody)
                .font(.body.monospaced())
                .frame(minHeight: 140)
                .overlay(
                    RoundedRectangle(cornerRadius: 4)
                        .stroke(Color.secondary.opacity(0.35), lineWidth: 0.5)
                )
            HStack {
                Spacer()
                Button("Annuler") {
                    whisperPrompt = false
                }
                .keyboardShortcut(.cancelAction)
                Button("Envoyer") {
                    let body = whisperBody
                    whisperPrompt = false
                    Task { await model.whisper(d.id, body: body) }
                }
                .keyboardShortcut(.defaultAction)
                .disabled(whisperBody.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }
        .padding(18)
        .frame(width: 420)
    }
}
