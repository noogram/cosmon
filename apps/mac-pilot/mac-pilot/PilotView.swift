//
//  PilotView.swift
//  mac-pilot
//
//  The popover content. v1 wires four tabs: Session (carnet), Whispers
//  (matrix ingress), Inbox (pending molecules), Galaxies (peer switch).
//  Keyboard-first — Enter = note, Cmd+S = start/end session, Cmd+1..4 =
//  pick tab, Esc = dismiss. Polling timers pause when the popover is
//  closed; every shell-out is a one-off `cs` invocation.
//

import SwiftUI
import AppKit

/// Top-level tab in the popover.
enum PilotTab: String, CaseIterable, Identifiable {
    case session  = "Session"
    case whispers = "Whispers"
    case inbox    = "Inbox"
    case galaxies = "Galaxies"
    case cluster  = "Cluster"
    var id: String { rawValue }
}

/// View-state for the Session pane. Owns the polling timer and the async shell-outs.
@MainActor
final class PilotViewModel: ObservableObject {

    enum Feedback: Equatable {
        case idle
        case busy(String)
        case success(String)
        case failure(String)
    }

    @Published private(set) var state: SessionState?
    @Published private(set) var feedback: Feedback = .idle
    @Published private(set) var lastRefresh: Date?

    /// Timestamps (`HH:MM:SS`) of notes in the current session that
    /// already have a sidecar under `.cosmon/state/sessions/.promoted/`.
    /// Used by `notesList` to hide the promote button for done notes.
    @Published private(set) var promotedTimestamps: Set<String> = []

    /// Short-lived map of note timestamp → most recent spark id, so the
    /// UI can flash a `spark-*` badge on a note right after promotion.
    @Published private(set) var recentSparkByTimestamp: [String: String] = [:]

    private var timer: Timer?

    func startPolling(every seconds: TimeInterval = 3) {
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
            let fresh = try await CosmonBridge.current()
            if fresh != state {
                state = fresh
            }
            // Refresh the promoted-sidecar set every tick — the
            // LaunchAgent may have promoted a `!spark`-prefixed note
            // in the background while the UI was dormant.
            if let sid = fresh?.sessionID {
                promotedTimestamps = CosmonBridge.promotedNoteTimestamps(sessionID: sid)
            } else {
                promotedTimestamps = []
            }
            lastRefresh = Date()
        } catch {
            // Non-fatal: keep last-known state; surface error only when the
            // operator takes an action.
        }
    }

    /// Promote the named note into a spark molecule. Returns the spark
    /// id (empty string if parsing fails but nucleation succeeded).
    @discardableResult
    func promoteNote(_ note: Note) async -> String {
        guard let sid = state?.sessionID else {
            feedback = .failure("Aucune session ouverte")
            scheduleFeedbackClear(after: 2.0)
            return ""
        }
        var sparkID = ""
        await perform(label: "Promotion…") {
            sparkID = try await CosmonBridge.promoteSessionNote(sessionID: sid, note: note)
            if sparkID.isEmpty {
                return "Spark créé"
            }
            return "Promue → \(sparkID)"
        }
        if !sparkID.isEmpty {
            recentSparkByTimestamp[note.timestamp] = sparkID
        }
        promotedTimestamps.insert(note.timestamp)
        await refresh()
        return sparkID
    }

    func startSession() async {
        await perform(label: "Démarrage…") {
            _ = try await CosmonBridge.start(galaxy: nil)
            return "Session démarrée"
        }
        await refresh()
    }

    func endSession() async {
        await perform(label: "Clôture…") {
            let seal = try await CosmonBridge.end()
            let short = Self.shortSeal(seal.hash)
            return "Session scellée — \(short)"
        }
        await refresh()
    }

    func addNote(text: String, tag: String?) async -> Bool {
        let ok = await performBool(label: "Envoi note…") {
            try await CosmonBridge.note(text, tag: tag)
            return "Note ajoutée"
        }
        await refresh()
        return ok
    }

    /// Universal Inbox drop from the menubar — shells into `cs drop`.
    ///
    /// The mac-pilot companion to the global Hammerspoon hotkey
    /// (`⌃⌥D`) and the zsh widget (`Ctrl-G`): same backend verb,
    /// different entry point. Input flows through the same
    /// `source:drop` auto-tag applied by the Rust verb, so later
    /// triage can still distinguish menubar origin via the caller-
    /// supplied `source:menubar` tag we send below.
    ///
    /// Returns `true` on success so the caller (the "Drop…" sheet)
    /// can dismiss; false on failure.
    func submitDrop(text: String) async -> Bool {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return false }
        return await performBool(label: "Drop…") {
            let id = try await CosmonBridge.drop(trimmed)
            return id.isEmpty ? "Drop dispatché" : "Drop → \(id)"
        }
    }

    // MARK: - Feedback helpers

    private func perform(label: String, _ op: @escaping () async throws -> String) async {
        feedback = .busy(label)
        do {
            let ok = try await op()
            feedback = .success(ok)
            scheduleFeedbackClear(after: 0.8)
        } catch {
            feedback = .failure(Self.errorMessage(error))
            scheduleFeedbackClear(after: 2.5)
        }
    }

    private func performBool(label: String, _ op: @escaping () async throws -> String) async -> Bool {
        feedback = .busy(label)
        do {
            let ok = try await op()
            feedback = .success(ok)
            scheduleFeedbackClear(after: 0.8)
            return true
        } catch {
            feedback = .failure(Self.errorMessage(error))
            scheduleFeedbackClear(after: 2.5)
            return false
        }
    }

    private func scheduleFeedbackClear(after seconds: TimeInterval) {
        Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: UInt64(seconds * 1_000_000_000))
            if case .busy = self?.feedback { return }
            self?.feedback = .idle
        }
    }

    private static func errorMessage(_ error: Error) -> String {
        if let cosmon = error as? CosmonError {
            return cosmon.errorDescription ?? "Erreur inconnue"
        }
        return error.localizedDescription
    }

    private static func shortSeal(_ hash: String) -> String {
        guard hash.hasPrefix("blake3:") else { return hash }
        let trimmed = hash.dropFirst("blake3:".count)
        return "blake3:\(trimmed.prefix(8))…"
    }
}

struct PilotView: View {
    @StateObject private var model = PilotViewModel()
    @StateObject private var whispersModel = WhispersViewModel()
    @StateObject private var inboxModel = InboxViewModel()
    @StateObject private var galaxiesModel = GalaxiesViewModel()
    @State private var noteText: String = ""
    @State private var tagText: String = ""
    @State private var tab: PilotTab = .session
    @State private var dropSheetShown: Bool = false
    @State private var dropText: String = ""
    @FocusState private var noteFieldFocused: Bool
    @FocusState private var dropFieldFocused: Bool

    /// Markdown theme selector. Shared with ios-pilot via the
    /// `"markdown_theme"` UserDefaults key, so a Mac+iPad operator
    /// keeps the same look across surfaces without extra sync.
    @AppStorage("markdown_theme") private var markdownThemeRaw: String = MarkdownThemeID.relaxed.rawValue

    private var markdownThemeBinding: Binding<MarkdownThemeID> {
        Binding(
            get: { MarkdownThemeID(rawValue: markdownThemeRaw) ?? .relaxed },
            set: { markdownThemeRaw = $0.rawValue }
        )
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            headerBar
            Divider()
            tabPicker
                .padding(.horizontal, 12)
                .padding(.vertical, 8)

            Group {
                switch tab {
                case .session:  sessionPane
                case .whispers: WhispersView(model: whispersModel)
                case .inbox:    InboxView(model: inboxModel)
                case .galaxies: GalaxiesView(model: galaxiesModel)
                case .cluster:  ClusterView()
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)

            Divider()
            footerBar
        }
        .frame(width: 340, height: 500)
        .background(hiddenShortcuts)
        .sheet(isPresented: $dropSheetShown) { dropSheet }
        .task {
            await model.refresh()
            model.startPolling()
            await whispersModel.refresh()
            whispersModel.startPolling()
            await inboxModel.refresh(force: true)
            inboxModel.startPolling()
            await galaxiesModel.refresh()
            galaxiesModel.startPolling()
            if model.state != nil { noteFieldFocused = true }
        }
        .onDisappear {
            model.stopPolling()
            whispersModel.stopPolling()
            inboxModel.stopPolling()
            galaxiesModel.stopPolling()
        }
        .onChange(of: tab) { _, newValue in
            // Re-pump when switching to a data-heavy tab.
            switch newValue {
            case .whispers: Task { await whispersModel.refresh() }
            case .inbox:    Task { await inboxModel.refresh() }
            case .galaxies: Task { await galaxiesModel.refresh() }
            case .session:  Task { await model.refresh() }
            case .cluster:  break // ClusterView owns its own polling
            }
        }
    }

    // MARK: - Tab picker (with badges)

    private var tabPicker: some View {
        HStack(spacing: 4) {
            ForEach(PilotTab.allCases) { t in
                tabButton(for: t)
            }
        }
    }

    private func tabButton(for t: PilotTab) -> some View {
        let selected = t == tab
        let label = badgeLabel(for: t)
        return Button {
            tab = t
        } label: {
            Text(label)
                .font(.caption.weight(selected ? .bold : .regular))
                .foregroundColor(selected ? .white : .primary)
                .padding(.horizontal, 8)
                .padding(.vertical, 4)
                .frame(maxWidth: .infinity)
                .background(
                    RoundedRectangle(cornerRadius: 5)
                        .fill(selected ? Color.accentColor : Color.secondary.opacity(0.12))
                )
        }
        .buttonStyle(.plain)
    }

    private func badgeLabel(for t: PilotTab) -> String {
        switch t {
        case .session:  return t.rawValue
        case .whispers:
            let n = whispersModel.unreadCount
            return n > 0 ? "\(t.rawValue) (\(n))" : t.rawValue
        case .inbox:
            let n = inboxModel.hotCount
            return n > 0 ? "\(t.rawValue) (\(n))" : t.rawValue
        case .galaxies: return t.rawValue
        case .cluster:  return t.rawValue
        }
    }

    /// Invisible buttons to register Cmd+1..5 shortcuts without showing a menu bar.
    private var hiddenShortcuts: some View {
        ZStack {
            Button("") { tab = .session }
                .keyboardShortcut("1", modifiers: [.command])
            Button("") { tab = .whispers }
                .keyboardShortcut("2", modifiers: [.command])
            Button("") { tab = .inbox }
                .keyboardShortcut("3", modifiers: [.command])
            Button("") { tab = .galaxies }
                .keyboardShortcut("4", modifiers: [.command])
            Button("") { tab = .cluster }
                .keyboardShortcut("5", modifiers: [.command])
        }
        .opacity(0)
        .frame(width: 0, height: 0)
    }

    // MARK: - Header

    private var headerBar: some View {
        HStack(spacing: 8) {
            Image(systemName: "safari")
                .foregroundColor(.accentColor)
            VStack(alignment: .leading, spacing: 2) {
                Text("cosmon").font(.headline)
                Text(statusText)
                    .font(.caption)
                    .foregroundColor(.secondary)
            }
            Spacer()
            feedbackIndicator
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 10)
    }

    private var statusText: String {
        if let s = model.state {
            return "Session ouverte depuis \(Self.timeFormatter.string(from: s.startedAt))"
        }
        return "Aucune session ouverte"
    }

    @ViewBuilder
    private var feedbackIndicator: some View {
        switch model.feedback {
        case .idle: EmptyView()
        case .busy:
            ProgressView().controlSize(.small)
        case .success:
            Image(systemName: "checkmark.circle.fill")
                .foregroundColor(.green)
                .help(feedbackText)
        case .failure:
            Image(systemName: "exclamationmark.triangle.fill")
                .foregroundColor(.red)
                .help(feedbackText)
        }
    }

    private var feedbackText: String {
        switch model.feedback {
        case .idle: return ""
        case .busy(let s), .success(let s), .failure(let s): return s
        }
    }

    // MARK: - Session pane

    private var sessionPane: some View {
        VStack(alignment: .leading, spacing: 10) {
            if model.state != nil {
                noteComposer
                notesList
            } else {
                emptySessionHint
            }
        }
        .padding(.horizontal, 12)
        .padding(.top, 4)
    }

    private var noteComposer: some View {
        VStack(alignment: .leading, spacing: 6) {
            TextField("Note (Entrée pour envoyer)", text: $noteText)
                .textFieldStyle(.roundedBorder)
                .focused($noteFieldFocused)
                .onSubmit { Task { await submitNote() } }
            HStack(spacing: 6) {
                TextField("tag (optionnel)", text: $tagText)
                    .textFieldStyle(.roundedBorder)
                    .frame(maxWidth: 120)
                Button("Note") {
                    Task { await submitNote() }
                }
                .keyboardShortcut(.return, modifiers: [.command])
                .disabled(noteText.trimmingCharacters(in: .whitespaces).isEmpty)
                Spacer()
            }
        }
    }

    private var notesList: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text("Dernières notes").font(.caption).foregroundColor(.secondary)
            if let s = model.state, !s.notes.isEmpty {
                ScrollView {
                    VStack(alignment: .leading, spacing: 6) {
                        ForEach(s.notes.suffix(5).reversed()) { note in
                            noteRow(note)
                        }
                    }
                }
            } else {
                Text("Aucune note encore.").font(.footnote).foregroundColor(.secondary)
            }
        }
    }

    /// One row in the `notesList` — timestamp + tag on the left, the
    /// note body under it, and a trailing "Promouvoir en spark" button
    /// that turns into a `spark-*` badge once the sidecar is present.
    ///
    /// The button is hidden when the note has already been promoted
    /// (either in this session via [`PilotViewModel.promoteNote`] or
    /// ambiently by the `session-to-spark` LaunchAgent scanning the
    /// `!spark ` prefix). Idempotence is enforced below the UI by the
    /// tick script's sidecar check — the button is just a friendly
    /// default.
    private func noteRow(_ note: Note) -> some View {
        let promoted = model.promotedTimestamps.contains(note.timestamp)
        let recent = model.recentSparkByTimestamp[note.timestamp]

        return VStack(alignment: .leading, spacing: 2) {
            HStack(spacing: 6) {
                Text(note.timestamp)
                    .font(.caption2.monospaced())
                    .foregroundColor(.secondary)
                if let t = note.tag {
                    Text(t)
                        .font(.caption2)
                        .padding(.horizontal, 4)
                        .padding(.vertical, 1)
                        .background(Color.accentColor.opacity(0.15))
                        .cornerRadius(3)
                }
                Spacer()
                if promoted {
                    Text(recent.map { "🚀 \($0)" } ?? "🚀 promue")
                        .font(.caption2.monospaced())
                        .foregroundColor(.secondary)
                        .help(recent.map { "Spark \($0) créée" } ?? "Note déjà promue en spark")
                } else {
                    Button {
                        Task { await model.promoteNote(note) }
                    } label: {
                        Image(systemName: "sparkles")
                            .font(.caption2)
                    }
                    .buttonStyle(.borderless)
                    .help("Promouvoir en spark (→ Inbox)")
                }
            }
            Text(note.text)
                .font(.footnote)
                .fixedSize(horizontal: false, vertical: true)
                .opacity(promoted ? 0.6 : 1.0)
        }
        .padding(.vertical, 2)
    }

    private var emptySessionHint: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Aucune session ouverte.")
                .font(.callout)
            Text("Clique **Start Session** (ou ⌘S) pour commencer un carnet.")
                .font(.footnote)
                .foregroundColor(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    // MARK: - Footer

    private var footerBar: some View {
        HStack(spacing: 8) {
            if model.state == nil {
                Button("Start Session") { Task { await model.startSession() } }
                    .keyboardShortcut("s", modifiers: [.command])
            } else {
                Button("End Session") { Task { await model.endSession() } }
                    .keyboardShortcut("s", modifiers: [.command])
            }

            Spacer()

            Button {
                dropText = ""
                dropSheetShown = true
                dropFieldFocused = true
            } label: {
                Label("Drop…", systemImage: "sparkles")
            }
            .help("Drop ✦ chord ⌃⌥D — lance un spark sans cérémonie (⌘D)")
            .keyboardShortcut("d", modifiers: [.command])

            themeMenu

            Button {
                openInTerminal()
            } label: {
                Label("Terminal", systemImage: "terminal")
            }
            .help("Ouvre Ghostty dans /srv/cosmon/cosmon")

            Button {
                NSApp.terminate(nil)
            } label: {
                Image(systemName: "power")
            }
            .help("Quitter mac-pilot")
            .keyboardShortcut("q", modifiers: [.command])
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
    }

    /// Popover footer dropdown for the markdown theme. Mirrors the
    /// iOS SettingsView picker so both surfaces expose the same
    /// knob at the same granularity. The choice persists via
    /// `@AppStorage("markdown_theme")` — the iPad app reads the same
    /// key, keeping the experience consistent across devices.
    private var themeMenu: some View {
        Menu {
            Picker("Thème markdown", selection: markdownThemeBinding) {
                ForEach(MarkdownThemeID.allCases) { id in
                    Text(id.label).tag(id)
                }
            }
        } label: {
            Image(systemName: "textformat")
        }
        .menuStyle(.borderlessButton)
        .fixedSize()
        .help("Thème markdown — topic + whispers")
    }

    // MARK: - Drop sheet

    /// Minimal input sheet for the "Drop…" menubar button.
    ///
    /// ADR-066 wheat-paste compliance: input-only. The sheet never
    /// renders cosmon state — it owns a single text field and flushes
    /// via `submitDrop` → `cs drop`. The resulting spark appears in
    /// the Inbox tab on the next refresh, rendered by the shared
    /// `cs peek --snapshot` pipeline like every other pilot surface.
    @ViewBuilder
    private var dropSheet: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 6) {
                Image(systemName: "sparkles")
                    .foregroundColor(.accentColor)
                Text("Drop")
                    .font(.headline)
                Spacer()
                Text("⌃⌥D / Ctrl-G")
                    .font(.caption2.monospaced())
                    .foregroundColor(.secondary)
            }
            TextField("Que s'est-il passé dans ta tête ?", text: $dropText)
                .textFieldStyle(.roundedBorder)
                .focused($dropFieldFocused)
                .onSubmit { Task { await submitDrop() } }
            HStack {
                Button("Annuler") { dropSheetShown = false }
                    .keyboardShortcut(.escape, modifiers: [])
                Spacer()
                Button("Drop") { Task { await submitDrop() } }
                    .keyboardShortcut(.return, modifiers: [])
                    .disabled(dropText.trimmingCharacters(in: .whitespaces).isEmpty)
            }
        }
        .padding(16)
        .frame(width: 360)
    }

    private func submitDrop() async {
        let trimmed = dropText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        let ok = await model.submitDrop(text: trimmed)
        if ok {
            dropText = ""
            dropSheetShown = false
        }
    }

    // MARK: - Actions

    private func submitNote() async {
        let trimmed = noteText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        let tag = tagText.trimmingCharacters(in: .whitespaces)
        let ok = await model.addNote(text: trimmed, tag: tag.isEmpty ? nil : tag)
        if ok {
            noteText = ""
            tagText = ""
            noteFieldFocused = true
        }
    }

    private func openInTerminal() {
        CosmonBridge.openInTerminal(galaxyPath: CosmonBridge.galaxyRoot)
    }

    // MARK: - Formatters

    private static let timeFormatter: DateFormatter = {
        let f = DateFormatter()
        f.dateFormat = "HH:mm"
        return f
    }()
}

#Preview {
    PilotView()
}
