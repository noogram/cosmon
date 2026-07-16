// SPDX-License-Identifier: MPL-2.0
//
// MoleculeDetailScreen — briefing on top, log tail underneath, optional
// tmux attach hint at the bottom.
//
// Layout per Verdict-door spirit (one decision per surface): the top
// strip carries identity (id, status, formula, step). When the
// molecule is `pending` and has unblocked deps, we render the body as
// a verdict-door card so the operator can recognise the *one* call to
// make. For everything else we fall back to a flat reading layout.

import SwiftUI
import CosmonAppKit

struct MoleculeDetailScreen: View {
    let galaxy: String
    let id: String

    @EnvironmentObject var store: ClusterStore
    @State private var detail: MoleculeDetail?
    @State private var fullLog: String?
    @State private var loadError: String?
    @State private var loadingDetail = false

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                if let detail {
                    HeaderCard(detail: detail)
                    if needsVerdictDoor(detail) {
                        VerdictDoorCard(detail: detail)
                    }
                    BriefingCard(text: detail.briefing)
                    LogCard(tail: fullLog ?? detail.logTail,
                            truncated: fullLog == nil && detail.logTruncated,
                            onLoadFull: { Task { await loadFullLog() } })
                    if let hint = detail.tmuxAttachHint {
                        TmuxHintCard(hint: hint)
                    }
                } else if loadingDetail {
                    ProgressView().frame(maxWidth: .infinity)
                } else if let err = loadError {
                    ErrorCard(message: err)
                } else {
                    ProgressView().frame(maxWidth: .infinity)
                }
            }
            .padding(16)
        }
        .navigationTitle(id)
        .navigationBarTitleDisplayMode(.inline)
        .background(CosmonPalette.bone)
        .task { await loadDetail() }
        .refreshable { await loadDetail() }
    }

    private func loadDetail() async {
        loadingDetail = true
        defer { loadingDetail = false }
        do {
            let d = try await store.loadDetail(galaxy: galaxy, id: id)
            self.detail = d
            self.loadError = nil
        } catch {
            self.loadError = "\(error)"
        }
    }

    private func loadFullLog() async {
        do {
            let log = try await store.loadLog(galaxy: galaxy, id: id)
            self.fullLog = log
        } catch {
            self.loadError = "\(error)"
        }
    }

    private func needsVerdictDoor(_ detail: MoleculeDetail) -> Bool {
        // V1 heuristic: molecules in `pending` status with no completed
        // steps are the ones that may demand attention. Everything else
        // is observation-only — show the flat reading layout.
        detail.status == "pending" && detail.completedSteps.isEmpty
    }
}

private struct HeaderCard: View {
    let detail: MoleculeDetail

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                Text(detail.kind ?? "molécule")
                    .font(.caption.weight(.medium))
                    .padding(.horizontal, 8)
                    .padding(.vertical, 3)
                    .background(CosmonPalette.indigo.opacity(0.12), in: Capsule())
                    .foregroundStyle(CosmonPalette.indigo)
                Text(detail.status)
                    .font(.caption.weight(.medium))
                    .padding(.horizontal, 8)
                    .padding(.vertical, 3)
                    .background(CosmonPalette.status(detail.status).opacity(0.18), in: Capsule())
                    .foregroundStyle(CosmonPalette.status(detail.status))
                Spacer()
                Text("step \(detail.currentStep + 1)/\(detail.totalSteps)")
                    .font(.caption.monospacedDigit())
                    .foregroundStyle(CosmonPalette.charcoal.opacity(0.6))
            }
            Text(detail.formula)
                .font(.subheadline.monospaced())
                .foregroundStyle(CosmonPalette.charcoal.opacity(0.8))
            if let topic = detail.variables["topic"], !topic.isEmpty {
                Text(topic)
                    .font(.body)
                    .foregroundStyle(CosmonPalette.charcoal)
                    .lineLimit(4)
                    .padding(.top, 4)
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: 10)
                .stroke(CosmonPalette.charcoal.opacity(0.10), lineWidth: 1)
        )
    }
}

private struct VerdictDoorCard: View {
    let detail: MoleculeDetail

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Une décision attend.")
                .font(.headline)
                .foregroundStyle(CosmonPalette.cadmium)
            Text("`cs tackle \(detail.id)` côté Mac pour démarrer.")
                .font(.subheadline.monospaced())
                .foregroundStyle(CosmonPalette.charcoal)
                .padding(.vertical, 4)
                .padding(.horizontal, 8)
                .background(CosmonPalette.cadmium.opacity(0.10), in: RoundedRectangle(cornerRadius: 6))
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: 12)
                .stroke(CosmonPalette.cadmium, lineWidth: 2)
        )
    }
}

private struct BriefingCard: View {
    let text: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("briefing")
                .font(.caption.weight(.medium))
                .foregroundStyle(CosmonPalette.charcoal.opacity(0.5))
            if let text, !text.isEmpty {
                Text(text)
                    .font(.body)
                    .foregroundStyle(CosmonPalette.charcoal)
                    .frame(maxWidth: .infinity, alignment: .leading)
            } else {
                Text("(pas de briefing)")
                    .font(.body.italic())
                    .foregroundStyle(CosmonPalette.charcoal.opacity(0.5))
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(CosmonPalette.boneShade.opacity(0.5), in: RoundedRectangle(cornerRadius: 10))
    }
}

private struct LogCard: View {
    let tail: String?
    let truncated: Bool
    let onLoadFull: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text("log")
                    .font(.caption.weight(.medium))
                    .foregroundStyle(CosmonPalette.charcoal.opacity(0.5))
                Spacer()
                if truncated {
                    Button("voir tout", action: onLoadFull)
                        .font(.caption)
                        .foregroundStyle(CosmonPalette.indigo)
                }
            }
            if let tail, !tail.isEmpty {
                Text(tail)
                    .font(.system(.footnote, design: .monospaced))
                    .foregroundStyle(CosmonPalette.charcoal)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .textSelection(.enabled)
            } else {
                Text("(pas de log)")
                    .font(.body.italic())
                    .foregroundStyle(CosmonPalette.charcoal.opacity(0.5))
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(CosmonPalette.charcoal.opacity(0.04), in: RoundedRectangle(cornerRadius: 10))
    }
}

private struct TmuxHintCard: View {
    let hint: String

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text("attach worker (Mac)")
                .font(.caption.weight(.medium))
                .foregroundStyle(CosmonPalette.charcoal.opacity(0.5))
            Text(hint)
                .font(.system(.subheadline, design: .monospaced))
                .foregroundStyle(CosmonPalette.indigo)
                .textSelection(.enabled)
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: 10)
                .stroke(CosmonPalette.indigo.opacity(0.20), lineWidth: 1)
        )
    }
}

private struct ErrorCard: View {
    let message: String

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text("Détail injoignable")
                .font(.headline)
                .foregroundStyle(CosmonPalette.cadmium)
            Text(message)
                .font(.footnote)
                .foregroundStyle(CosmonPalette.charcoal.opacity(0.7))
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(CosmonPalette.cadmium.opacity(0.10), in: RoundedRectangle(cornerRadius: 10))
    }
}

#Preview {
    let store = ClusterStore(client: MockDaemonClient())
    return NavigationStack {
        MoleculeDetailScreen(galaxy: "cosmon", id: "task-20260426-aaaa")
            .environmentObject(store)
    }
}
