//
//  SkylightView.swift
//  mac-pilot
//
//  Per-galaxy whisper window (Wheeler's Skylight — petite, asymétrique,
//  orientée). Hosted in `WindowGroup(for: URL.self)`, keyed by galaxy path,
//  so multiple Skylights coexist side-by-side. Reads the galaxy's Matrix
//  inbox and emits whispers via `cs whisper --message` scoped to the
//  galaxy's cwd. Monospaced-only rendering — wheat-paste §8k' compliance.
//

import SwiftUI

@MainActor
final class SkylightViewModel: ObservableObject {

    enum Feedback: Equatable {
        case idle, busy(String), success(String), failure(String)
    }

    let galaxyPath: URL

    @Published private(set) var whispers: [Whisper] = []
    @Published private(set) var runningMolecules: [MoleculeSummary] = []
    @Published private(set) var feedback: Feedback = .idle

    private var timer: Timer?

    var galaxyName: String { galaxyPath.lastPathComponent }

    init(galaxyPath: URL) { self.galaxyPath = galaxyPath }

    func startPolling(every seconds: TimeInterval = 5) {
        stopPolling()
        let t = Timer.scheduledTimer(withTimeInterval: seconds, repeats: true) { [weak self] _ in
            Task { @MainActor in await self?.refresh() }
        }
        RunLoop.main.add(t, forMode: .common)
        timer = t
    }

    func stopPolling() { timer?.invalidate(); timer = nil }

    func refresh() async {
        async let ws = loadWhispers()
        async let ms = loadRunningMolecules()
        let (fw, fm) = await (ws, ms)
        if fw != whispers { whispers = fw }
        if fm != runningMolecules { runningMolecules = fm }
    }

    private func loadWhispers() async -> [Whisper] {
        let inbox = galaxyPath.appendingPathComponent(".cosmon/whispers/inbox", isDirectory: true)
        do {
            let fresh = try await CosmonBridge.listWhispers(inboxRoot: inbox)
            return Array(fresh.prefix(100))
        } catch { return whispers }
    }

    private func loadRunningMolecules() async -> [MoleculeSummary] {
        do {
            return try await CosmonBridge.listInboxIn(galaxyPath: galaxyPath)
                .filter { $0.status == "running" || $0.status == "active" }
        } catch { return runningMolecules }
    }

    func emit(targetID: String, body: String) async -> Bool {
        let trimmed = body.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, !targetID.isEmpty else { return false }
        feedback = .busy("cs whisper…")
        do {
            try await CosmonBridge.whisper(
                galaxyPath: galaxyPath, moleculeID: targetID, body: trimmed
            )
            feedback = .success("whisper émis → \(targetID)")
            clearFeedback(after: 1.5)
            return true
        } catch {
            feedback = .failure(Self.errorMessage(error))
            clearFeedback(after: 3.0)
            return false
        }
    }

    private func clearFeedback(after seconds: TimeInterval) {
        Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: UInt64(seconds * 1_000_000_000))
            if case .busy = self?.feedback { return }
            self?.feedback = .idle
        }
    }

    private static func errorMessage(_ e: Error) -> String {
        (e as? CosmonError)?.errorDescription ?? e.localizedDescription
    }
}

struct SkylightView: View {
    @StateObject private var model: SkylightViewModel
    @State private var bodyText: String = ""
    @State private var selectedTarget: String = ""
    @FocusState private var composerFocused: Bool

    init(galaxyPath: URL) {
        _model = StateObject(wrappedValue: SkylightViewModel(galaxyPath: galaxyPath))
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            header
            Divider()
            feed
            Divider()
            composer
        }
        .padding(12)
        .frame(minWidth: 440, idealWidth: 520, minHeight: 420, idealHeight: 520)
        .task {
            await model.refresh()
            model.startPolling()
        }
        .onDisappear { model.stopPolling() }
    }

    private var header: some View {
        HStack(alignment: .firstTextBaseline) {
            Text("skylight://\(model.galaxyName)")
                .font(.system(.headline, design: .monospaced))
            Spacer()
            feedbackLine
        }
    }

    @ViewBuilder
    private var feedbackLine: some View {
        switch model.feedback {
        case .idle:         monoCaption("·", color: .secondary)
        case .busy(let s):  monoCaption(s, color: .secondary)
        case .success(let s): monoCaption(s, color: .green)
        case .failure(let s): monoCaption(s, color: .red)
        }
    }

    private func monoCaption(_ s: String, color: Color) -> some View {
        Text(s).font(.caption2.monospaced()).foregroundColor(color)
    }

    private var feed: some View {
        VStack(alignment: .leading, spacing: 4) {
            monoCaption("whispers · \(model.whispers.count)", color: .secondary)
            if model.whispers.isEmpty {
                Text("(aucun whisper dans .cosmon/whispers/inbox)")
                    .font(.footnote.monospaced())
                    .foregroundColor(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
            } else {
                ScrollView {
                    VStack(alignment: .leading, spacing: 2) {
                        ForEach(model.whispers) { w in whisperLine(w) }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
        }
        .frame(minHeight: 180, maxHeight: .infinity, alignment: .topLeading)
    }

    private func whisperLine(_ w: Whisper) -> some View {
        let ts = Self.hhmm(from: w.receivedAt)
        let sender = w.senderNucleonID == "?" ? "anon" : w.senderNucleonID
        let prev = w.preview.isEmpty ? "(vide)" : w.preview
        return Text("\(ts)  \(sender)  \(prev)")
            .font(.system(.footnote, design: .monospaced))
            .fixedSize(horizontal: false, vertical: true)
            .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var composer: some View {
        VStack(alignment: .leading, spacing: 6) {
            monoCaption("émettre →", color: .secondary)
            if model.runningMolecules.isEmpty {
                Text("(aucune molécule running dans \(model.galaxyName))")
                    .font(.footnote.monospaced())
                    .foregroundColor(.secondary)
            } else {
                Picker("", selection: $selectedTarget) {
                    Text("— choisir une cible —").tag("")
                    ForEach(model.runningMolecules) { m in
                        Text("\(m.id)  ·  \(m.formula)").tag(m.id)
                    }
                }
                .labelsHidden()
                .pickerStyle(.menu)
                .font(.footnote.monospaced())
            }
            TextEditor(text: $bodyText)
                .font(.system(.footnote, design: .monospaced))
                .frame(minHeight: 60, idealHeight: 80, maxHeight: 120)
                .focused($composerFocused)
                .disableAutocorrection(true)
                .overlay(alignment: .topLeading) {
                    if bodyText.isEmpty {
                        Text("corps du whisper…")
                            .font(.footnote.monospaced())
                            .foregroundColor(.secondary)
                            .padding(6)
                            .allowsHitTesting(false)
                    }
                }
            HStack {
                monoCaption("⌘⏎ send", color: .secondary)
                Spacer()
                Button("send whisper") { Task { await submit() } }
                    .keyboardShortcut(.return, modifiers: [.command])
                    .disabled(!canSend)
            }
        }
    }

    private var canSend: Bool {
        !selectedTarget.isEmpty
            && !bodyText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    private func submit() async {
        guard canSend else { return }
        if await model.emit(targetID: selectedTarget, body: bodyText) {
            bodyText = ""
            composerFocused = true
            await model.refresh()
        }
    }

    private static let hhmmFormatter: DateFormatter = {
        let f = DateFormatter(); f.dateFormat = "HH:mm"; return f
    }()
    private static func hhmm(from d: Date) -> String { hhmmFormatter.string(from: d) }
}

#Preview {
    SkylightView(galaxyPath: URL(fileURLWithPath: NSHomeDirectory())
        .appendingPathComponent("galaxies/cosmon"))
}
