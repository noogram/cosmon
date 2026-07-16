//
//  WhispersView.swift
//  mac-pilot
//
//  Reads .cosmon/whispers/inbox/<room>/*.md directly from the filesystem —
//  no shell-out, no daemon. The pane polls every 5 s while the popover is
//  visible; clicking a whisper opens a detail pane with body + frontmatter
//  + "Transformer en task" (shell-out `cs spark`) + "Marquer lu" (move to
//  archived/).
//

import SwiftUI
import AppKit

@MainActor
final class WhispersViewModel: ObservableObject {

    enum Feedback: Equatable {
        case idle
        case busy(String)
        case success(String)
        case failure(String)
    }

    @Published private(set) var whispers: [Whisper] = []
    @Published var selected: Whisper?
    @Published private(set) var feedback: Feedback = .idle
    @Published private(set) var lastRefresh: Date?

    private var timer: Timer?

    var unreadCount: Int { whispers.count }

    func startPolling(every seconds: TimeInterval = 5) {
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
            let fresh = try await CosmonBridge.listWhispers()
            if fresh != whispers {
                whispers = fresh
                if let sel = selected, !fresh.contains(where: { $0.url == sel.url }) {
                    selected = nil
                }
            }
            lastRefresh = Date()
        } catch {
            // Silent — we re-try next tick.
        }
    }

    func transformToSpark(_ w: Whisper) async {
        feedback = .busy("cs spark…")
        do {
            let newID = try await CosmonBridge.transformWhisperToSpark(w)
            feedback = .success(newID.isEmpty ? "Spark créé" : "Spark \(newID) créé")
            try? await CosmonBridge.archiveWhisper(w)
            selected = nil
            await refresh()
            scheduleFeedbackClear(after: 1.5)
        } catch {
            feedback = .failure(errorMessage(error))
            scheduleFeedbackClear(after: 3.0)
        }
    }

    func archive(_ w: Whisper) async {
        feedback = .busy("Archivage…")
        do {
            try await CosmonBridge.archiveWhisper(w)
            feedback = .success("Whisper archivé")
            selected = nil
            await refresh()
            scheduleFeedbackClear(after: 1.0)
        } catch {
            feedback = .failure(errorMessage(error))
            scheduleFeedbackClear(after: 3.0)
        }
    }

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

struct WhispersView: View {
    @ObservedObject var model: WhispersViewModel
    @AppStorage("markdown_theme") private var markdownThemeRaw: String = MarkdownThemeID.relaxed.rawValue

    private var markdownTheme: MarkdownTheme {
        (MarkdownThemeID(rawValue: markdownThemeRaw) ?? .relaxed).theme
    }

    var body: some View {
        Group {
            if let w = model.selected {
                detail(for: w)
            } else {
                list
            }
        }
        .padding(.horizontal, 12)
        .padding(.top, 4)
    }

    private var list: some View {
        VStack(alignment: .leading, spacing: 6) {
            header
            if model.whispers.isEmpty {
                emptyState
            } else {
                ScrollView {
                    VStack(alignment: .leading, spacing: 4) {
                        ForEach(model.whispers) { w in
                            Button {
                                model.selected = w
                            } label: {
                                whisperRow(w)
                            }
                            .buttonStyle(.plain)
                            Divider()
                        }
                    }
                    .padding(.top, 2)
                }
            }
            Spacer(minLength: 0)
        }
    }

    private var header: some View {
        HStack {
            Text("Whispers")
                .font(.headline)
            Spacer()
            feedbackIndicator
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
            Text(s).font(.caption2).foregroundColor(.red)
        }
    }

    private var emptyState: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Aucun whisper.")
                .font(.callout)
            Text("Envoie un message dans `#cosmon-whispers` depuis Element.")
                .font(.footnote)
                .foregroundColor(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            Spacer()
        }
        .padding(.top, 8)
    }

    private func whisperRow(_ w: Whisper) -> some View {
        HStack(alignment: .top, spacing: 8) {
            Text("[\(w.senderNucleonID)]")
                .font(.caption.monospaced())
                .foregroundColor(.accentColor)
            VStack(alignment: .leading, spacing: 2) {
                if w.preview.isEmpty {
                    Text("(vide)")
                        .font(.footnote)
                        .foregroundColor(.secondary)
                } else {
                    let truncated = MarkdownView.truncatedMarkdown(w.preview, maxChars: 160)
                    MarkdownView(text: truncated, theme: .compact)
                        .lineLimit(2)
                }
                Text(Self.relativeTime(from: w.receivedAt))
                    .font(.caption2)
                    .foregroundColor(.secondary)
            }
            Spacer()
        }
        .padding(.vertical, 3)
        .contentShape(Rectangle())
    }

    private func detail(for w: Whisper) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Button {
                    model.selected = nil
                } label: {
                    Label("Retour", systemImage: "chevron.left")
                }
                .buttonStyle(.plain)
                Spacer()
                feedbackIndicator
            }

            VStack(alignment: .leading, spacing: 2) {
                Text(w.senderNucleonID)
                    .font(.headline)
                Text(w.senderMxID)
                    .font(.caption2.monospaced())
                    .foregroundColor(.secondary)
                Text(Self.relativeTime(from: w.receivedAt))
                    .font(.caption2)
                    .foregroundColor(.secondary)
            }

            Divider()

            ScrollView {
                VStack(alignment: .leading, spacing: 6) {
                    if w.body.isEmpty {
                        Text("(corps vide)")
                            .font(.footnote)
                            .foregroundColor(.secondary)
                    } else {
                        MarkdownView(text: w.body, theme: markdownTheme)
                            .textSelection(.enabled)
                    }

                    if !w.frontmatter.isEmpty {
                        Text("Metadata")
                            .font(.caption.bold())
                            .foregroundColor(.secondary)
                            .padding(.top, 4)
                        ForEach(Array(w.frontmatter.keys).sorted(), id: \.self) { k in
                            HStack(alignment: .top, spacing: 4) {
                                Text(k)
                                    .font(.caption2.monospaced())
                                    .foregroundColor(.secondary)
                                Text(w.frontmatter[k] ?? "")
                                    .font(.caption2.monospaced())
                                    .fixedSize(horizontal: false, vertical: true)
                            }
                        }
                    }
                }
            }

            Divider()

            HStack(spacing: 6) {
                Button {
                    Task { await model.transformToSpark(w) }
                } label: {
                    Label("Transformer en task", systemImage: "sparkles")
                }
                Button {
                    Task { await model.archive(w) }
                } label: {
                    Label("Marquer lu", systemImage: "checkmark")
                }
                Spacer()
            }
            .padding(.bottom, 4)
        }
    }

    private static func relativeTime(from date: Date) -> String {
        let seconds = Int(Date().timeIntervalSince(date))
        if seconds < 60 { return "à l'instant" }
        if seconds < 3600 {
            let m = seconds / 60
            return "il y a \(m)m"
        }
        if seconds < 86_400 {
            let h = seconds / 3600
            return "il y a \(h)h"
        }
        let d = seconds / 86_400
        return "il y a \(d)j"
    }
}
