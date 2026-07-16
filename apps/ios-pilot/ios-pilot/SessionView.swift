import SwiftUI
#if canImport(UIKit)
import UIKit
#endif

/// Predefined tags the operator can stamp onto a note. Freeform tags are
/// out of scope for v0 — picker keeps the keyboard path fast.
private let availableTags: [String] = ["idee", "decision", "todo", "spark"]

struct SessionView: View {
    @EnvironmentObject var session: SessionStore
    @EnvironmentObject var settings: SettingsStore

    @State private var draft: String = ""
    @State private var selectedTag: String? = nil
    @FocusState private var editorFocused: Bool

    var body: some View {
        NavigationStack {
            ZStack(alignment: .top) {
                content
                if session.connectivity == .offline {
                    offlineBanner
                        .transition(.move(edge: .top).combined(with: .opacity))
                }
            }
            .animation(.easeInOut, value: session.connectivity)
            .navigationTitle("Cosmon")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Circle()
                        .fill(session.connectivity.color)
                        .frame(width: 10, height: 10)
                        .accessibilityLabel(Text("connectivity"))
                }
            }
            .task {
                await session.refresh()
                if settings.pollingEnabled {
                    session.startPolling(interval: settings.pollingInterval)
                }
                editorFocused = true
            }
            .onChange(of: settings.pollingEnabled) { _, enabled in
                if enabled {
                    session.startPolling(interval: settings.pollingInterval)
                } else {
                    session.stopPolling()
                }
            }
        }
    }

    @ViewBuilder
    private var content: some View {
        VStack(alignment: .leading, spacing: 16) {
            statusHeader
                .padding(.horizontal)
                .padding(.top, session.connectivity == .offline ? 52 : 8)

            composer
                .padding(.horizontal)

            actionRow
                .padding(.horizontal)

            Divider()

            notesSection
        }
        .padding(.bottom)
    }

    private var statusHeader: some View {
        VStack(alignment: .leading, spacing: 4) {
            if let sid = session.state.sessionID?.value {
                Text("Session ouverte")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Text(sid)
                    .font(.headline)
                    .lineLimit(1)
                    .truncationMode(.middle)
            } else {
                Text("Aucune session")
                    .font(.headline)
                    .foregroundStyle(.secondary)
            }
            if !session.pending.isEmpty {
                Text("\(session.pending.count) note\(session.pending.count > 1 ? "s" : "") en file d'attente")
                    .font(.caption)
                    .foregroundStyle(.orange)
            }
            if let err = session.lastError {
                Text(err)
                    .font(.caption2)
                    .foregroundStyle(.red)
            }
        }
    }

    private var composer: some View {
        VStack(alignment: .leading, spacing: 8) {
            TextField("Note…", text: $draft, axis: .vertical)
                .textFieldStyle(.roundedBorder)
                .lineLimit(1...6)
                .focused($editorFocused)
                .onSubmit {
                    if canSend { submit() }
                }
                .submitLabel(.send)

            HStack {
                Menu {
                    Button("Aucun tag") { selectedTag = nil }
                    Divider()
                    ForEach(availableTags, id: \.self) { t in
                        Button(t) { selectedTag = t }
                    }
                } label: {
                    Label(selectedTag ?? "Aucun tag", systemImage: "tag")
                        .font(.subheadline)
                }
                Spacer()
                Button(action: submit) {
                    Label("Envoyer", systemImage: "paperplane.fill")
                        .labelStyle(.titleAndIcon)
                }
                .buttonStyle(.borderedProminent)
                .disabled(!canSend)
            }
        }
    }

    private var actionRow: some View {
        HStack {
            if session.hasSession {
                Button(role: .destructive, action: endSession) {
                    Label("Fermer session", systemImage: "stop.circle")
                }
                .buttonStyle(.bordered)
            } else {
                Button(action: startSession) {
                    Label("Démarrer session", systemImage: "play.circle")
                }
                .buttonStyle(.borderedProminent)
                .disabled(session.isBusy)
            }
            Spacer()
        }
    }

    @ViewBuilder
    private var notesSection: some View {
        let recent = Array(session.state.notes.suffix(5).reversed())
        if recent.isEmpty && session.pending.isEmpty {
            VStack(spacing: 8) {
                Image(systemName: "square.and.pencil")
                    .font(.system(size: 42))
                    .foregroundStyle(.secondary)
                Text("Les cinq dernières notes apparaîtront ici.")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .padding()
        } else {
            List {
                if !session.pending.isEmpty {
                    Section("En attente d'envoi") {
                        ForEach(session.pending) { pending in
                            VStack(alignment: .leading, spacing: 4) {
                                Text(pending.text).font(.body)
                                HStack {
                                    if let t = pending.tag {
                                        Text("#\(t)").font(.caption).foregroundStyle(.secondary)
                                    }
                                    Text(pending.enqueuedAt, style: .time)
                                        .font(.caption2)
                                        .foregroundStyle(.secondary)
                                }
                            }
                        }
                    }
                }
                Section("Notes récentes") {
                    ForEach(recent, id: \.ts) { note in
                        noteRow(note)
                    }
                }
            }
            .listStyle(.insetGrouped)
            .refreshable { await session.refresh() }
        }
    }

    /// One row in the "Notes récentes" list — timestamp + body + tag
    /// + a "Promouvoir en spark" button that becomes a `spark-*` badge
    /// once the note has been promoted. The button is hidden on an
    /// already-promoted row (tracked client-side in `SessionStore`).
    @ViewBuilder
    private func noteRow(_ note: Note) -> some View {
        let promoted = session.promotedTimestamps.contains(note.ts)
        let recent = session.recentSparkByTimestamp[note.ts]

        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Text(note.text)
                    .font(.body)
                    .foregroundStyle(promoted ? .secondary : .primary)
                Spacer()
                if promoted {
                    Text(recent.map { "🚀 \($0)" } ?? "🚀 promue")
                        .font(.caption2.monospaced())
                        .foregroundStyle(.secondary)
                        .accessibilityLabel(
                            Text(recent.map { "Spark \($0) créée" } ?? "Note déjà promue")
                        )
                } else {
                    Button {
                        Task {
                            let id = await session.promote(note: note)
                            feedback(success: !id.isEmpty && session.lastError == nil)
                        }
                    } label: {
                        Image(systemName: "sparkles")
                            .font(.body)
                    }
                    .buttonStyle(.borderless)
                    .disabled(session.isBusy)
                    .accessibilityLabel(Text("Promouvoir en spark"))
                }
            }
            HStack {
                if let t = note.tag {
                    Text("#\(t)").font(.caption).foregroundStyle(.secondary)
                }
                Text(shortTime(note.ts))
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }
        }
    }

    private var offlineBanner: some View {
        Text("cs-api injoignable. Démarre-le sur ton Mac ou vérifie Tailscale.")
            .font(.footnote)
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .frame(maxWidth: .infinity)
            .background(Color.red.opacity(0.9))
            .foregroundStyle(.white)
    }

    private var canSend: Bool {
        !draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && (session.hasSession || session.connectivity == .offline)
            && !session.isBusy
    }

    private func submit() {
        let snapshot = draft
        let tag = selectedTag
        draft = ""
        editorFocused = true
        Task {
            let before = session.connectivity
            await session.send(text: snapshot, tag: tag)
            feedback(success: session.lastError == nil && (session.connectivity != .offline || before == .offline))
        }
    }

    private func startSession() {
        Task {
            await session.start()
            feedback(success: session.lastError == nil)
        }
    }

    private func endSession() {
        Task {
            await session.end()
            feedback(success: session.lastError == nil)
        }
    }

    private func feedback(success: Bool) {
        #if canImport(UIKit)
        let generator = UINotificationFeedbackGenerator()
        generator.notificationOccurred(success ? .success : .error)
        #endif
    }

    private func shortTime(_ iso: String) -> String {
        let iso8601 = ISO8601DateFormatter()
        iso8601.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        if let d = iso8601.date(from: iso) {
            return d.formatted(date: .omitted, time: .shortened)
        }
        iso8601.formatOptions = [.withInternetDateTime]
        if let d = iso8601.date(from: iso) {
            return d.formatted(date: .omitted, time: .shortened)
        }
        return iso
    }
}

#Preview {
    SessionView()
        .environmentObject(SessionStore(api: MockCosmonAPI()))
        .environmentObject(SettingsStore())
}
