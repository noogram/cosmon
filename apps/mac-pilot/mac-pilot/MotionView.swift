//
//  MotionView.swift
//  mac-pilot
//
//  "Molécules en mouvement" — live view of what every galaxy in the
//  local cluster is doing right now. Shell-outs to `cs motion --json`
//  every 3 seconds while the popover is open; pauses when dismissed.
//
//  Five collapsible sections: workers, running molecules, recent git
//  commits, recent whispers, recent sparks. One keystroke-free surface
//  — the operator's job is to look at it, not to drive it.
//

import SwiftUI
import AppKit

@MainActor
final class MotionViewModel: ObservableObject {
    @Published private(set) var snapshot: MotionSnapshot = .empty
    @Published private(set) var lastRefresh: Date?
    @Published private(set) var lastError: String?

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
            let fresh = try await CosmonBridge.motion()
            if fresh != snapshot { snapshot = fresh }
            lastError = nil
            lastRefresh = Date()
        } catch {
            // Keep last-known snapshot; store the message for the footer.
            if let cosmon = error as? CosmonError {
                lastError = cosmon.errorDescription ?? "erreur inconnue"
            } else {
                lastError = error.localizedDescription
            }
        }
    }
}

struct MotionView: View {
    @ObservedObject var model: MotionViewModel
    @State private var expanded: Set<MotionSection> = Set(MotionSection.allCases)

    enum MotionSection: String, CaseIterable, Identifiable, Hashable {
        case workers   = "Workers"
        case molecules = "Running molecules"
        case commits   = "Recent commits"
        case whispers  = "Recent whispers"
        case sparks    = "Recent sparks"
        var id: String { rawValue }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            header
            ScrollView {
                VStack(alignment: .leading, spacing: 10) {
                    section(.workers, count: model.snapshot.workers.count) {
                        workersBody
                    }
                    section(.molecules, count: model.snapshot.runningMolecules.count) {
                        moleculesBody
                    }
                    section(.commits, count: model.snapshot.recentCommits.count) {
                        commitsBody
                    }
                    section(.whispers, count: model.snapshot.recentWhispers.count) {
                        whispersBody
                    }
                    section(.sparks, count: model.snapshot.recentSparks.count) {
                        sparksBody
                    }
                }
                .padding(.top, 2)
            }
            if let err = model.lastError {
                Text(err)
                    .font(.caption2)
                    .foregroundColor(.red)
                    .padding(.top, 2)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .padding(.horizontal, 12)
        .padding(.top, 4)
    }

    // MARK: - Header

    private var header: some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack {
                Text("Motion").font(.headline)
                Spacer()
                if let ts = model.lastRefresh {
                    Text(Self.timeFormatter.string(from: ts))
                        .font(.caption2)
                        .foregroundColor(.secondary)
                }
            }
            Text("\(model.snapshot.galaxiesScanned.count) galaxies · window \(model.snapshot.window.isEmpty ? "—" : model.snapshot.window)")
                .font(.caption2)
                .foregroundColor(.secondary)
        }
    }

    // MARK: - Collapsible section shell

    @ViewBuilder
    private func section<Body: View>(
        _ s: MotionSection,
        count: Int,
        @ViewBuilder content: () -> Body
    ) -> some View {
        let open = expanded.contains(s)
        VStack(alignment: .leading, spacing: 4) {
            Button {
                if open {
                    expanded.remove(s)
                } else {
                    expanded.insert(s)
                }
            } label: {
                HStack(spacing: 6) {
                    Image(systemName: open ? "chevron.down" : "chevron.right")
                        .font(.caption2)
                        .foregroundColor(.secondary)
                    Text(s.rawValue).font(.subheadline.weight(.semibold))
                    Text("(\(count))").font(.caption).foregroundColor(.secondary)
                    Spacer()
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            if open {
                if count == 0 {
                    Text("—").font(.caption).foregroundColor(.secondary).padding(.leading, 14)
                } else {
                    content().padding(.leading, 4)
                }
            }
        }
    }

    // MARK: - Section bodies

    private var workersBody: some View {
        VStack(alignment: .leading, spacing: 3) {
            ForEach(model.snapshot.workers) { w in
                HStack(spacing: 6) {
                    statusDot(w.status)
                    Text(w.name).font(.caption.monospaced())
                    Text("[\(w.galaxy)]").font(.caption2).foregroundColor(.secondary)
                    if let mol = w.moleculeID {
                        Text(mol).font(.caption2).foregroundColor(.accentColor)
                    }
                    Spacer()
                    if let hb = w.lastHeartbeat {
                        Text(Self.shortTime(hb))
                            .font(.caption2.monospaced())
                            .foregroundColor(.secondary)
                    }
                }
            }
        }
    }

    private var moleculesBody: some View {
        VStack(alignment: .leading, spacing: 3) {
            ForEach(model.snapshot.runningMolecules) { m in
                VStack(alignment: .leading, spacing: 2) {
                    HStack(spacing: 6) {
                        Text(m.id).font(.caption.monospaced())
                        Text("[\(m.galaxy)]").font(.caption2).foregroundColor(.secondary)
                        Text(m.stepLabel)
                            .font(.caption2)
                            .foregroundColor(.orange)
                        Spacer()
                        if let evolve = m.lastEvolveAt {
                            Text(Self.shortTime(evolve))
                                .font(.caption2.monospaced())
                                .foregroundColor(.secondary)
                        }
                    }
                    if let topic = m.topicPreview, !topic.isEmpty {
                        Text(topic)
                            .font(.caption2)
                            .foregroundColor(.secondary)
                            .lineLimit(2)
                    }
                }
            }
        }
    }

    private var commitsBody: some View {
        VStack(alignment: .leading, spacing: 3) {
            ForEach(model.snapshot.recentCommits) { c in
                HStack(alignment: .top, spacing: 6) {
                    Text(c.sha).font(.caption2.monospaced()).foregroundColor(.orange)
                    Text("[\(c.galaxy)]").font(.caption2).foregroundColor(.secondary)
                    Text(c.subject).font(.caption).lineLimit(2)
                    Spacer()
                    Text(Self.shortTime(c.timestamp))
                        .font(.caption2.monospaced())
                        .foregroundColor(.secondary)
                }
                .contentShape(Rectangle())
                .onTapGesture {
                    copyToClipboard(c.sha)
                }
                .help("Tap to copy \(c.sha)")
            }
        }
    }

    private var whispersBody: some View {
        VStack(alignment: .leading, spacing: 3) {
            ForEach(model.snapshot.recentWhispers) { w in
                VStack(alignment: .leading, spacing: 2) {
                    HStack(spacing: 6) {
                        Text("[\(w.galaxy)]").font(.caption2).foregroundColor(.secondary)
                        if let sender = w.senderNucleonID {
                            Text(sender).font(.caption2).foregroundColor(.accentColor)
                        }
                        Spacer()
                        Text(Self.shortTime(w.receivedAt))
                            .font(.caption2.monospaced())
                            .foregroundColor(.secondary)
                    }
                    Text(w.bodyPreview).font(.caption).lineLimit(2)
                }
            }
        }
    }

    private var sparksBody: some View {
        VStack(alignment: .leading, spacing: 3) {
            ForEach(model.snapshot.recentSparks) { s in
                VStack(alignment: .leading, spacing: 2) {
                    HStack(spacing: 6) {
                        Text(s.id).font(.caption.monospaced())
                        Text("[\(s.galaxy)]").font(.caption2).foregroundColor(.secondary)
                        Spacer()
                        Text(Self.shortTime(s.createdAt))
                            .font(.caption2.monospaced())
                            .foregroundColor(.secondary)
                    }
                    if let topic = s.topicPreview, !topic.isEmpty {
                        Text(topic)
                            .font(.caption2)
                            .foregroundColor(.secondary)
                            .lineLimit(2)
                    }
                }
            }
        }
    }

    // MARK: - Status dot

    private func statusDot(_ status: String?) -> some View {
        let color: Color
        switch status?.lowercased() {
        case "active", "healthy", "running":
            color = .green
        case "stopped", "stale":
            color = .yellow
        case "error", "diverged":
            color = .red
        default:
            color = .gray
        }
        return Circle()
            .fill(color)
            .frame(width: 7, height: 7)
    }

    // MARK: - Helpers

    private func copyToClipboard(_ s: String) {
        let pb = NSPasteboard.general
        pb.clearContents()
        pb.setString(s, forType: .string)
    }

    private static let timeFormatter: DateFormatter = {
        let f = DateFormatter()
        f.dateFormat = "HH:mm:ss"
        return f
    }()

    private static let iso: ISO8601DateFormatter = {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime]
        return f
    }()

    /// Best-effort ISO-8601 → `HH:mm:ss`. Returns the raw string when
    /// parsing fails, so the UI never hides data behind format drift.
    static func shortTime(_ iso8601: String) -> String {
        if iso8601.isEmpty { return "-" }
        if let d = iso.date(from: iso8601) {
            return timeFormatter.string(from: d)
        }
        var relaxed = ISO8601DateFormatter()
        relaxed.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        if let d = relaxed.date(from: iso8601) {
            return timeFormatter.string(from: d)
        }
        return iso8601
    }
}
