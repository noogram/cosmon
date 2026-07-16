//
//  GalaxiesView.swift
//  mac-pilot
//
//  Scans /srv/cosmon/*/ for every directory containing a `.cosmon/` subdir,
//  shows a compact list with pending molecule count + last activity, and
//  offers an "Ouvrir dans terminal" action per galaxy.
//
//  v1 limitation: the app still reads session/whispers/inbox from
//  /srv/cosmon/cosmon/ only. Switching the "active" galaxy at runtime is a
//  v2 concern — here we just surface the peers so the operator can jump to
//  one in a terminal.
//

import SwiftUI
import AppKit

@MainActor
final class GalaxiesViewModel: ObservableObject {
    @Published private(set) var galaxies: [Galaxy] = []
    @Published private(set) var lastRefresh: Date?

    private var timer: Timer?

    func startPolling(every seconds: TimeInterval = 30) {
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
            let fresh = try await CosmonBridge.listGalaxies()
            if fresh != galaxies { galaxies = fresh }
            lastRefresh = Date()
        } catch {
            // Silent — re-try next tick.
        }
    }

    func openInTerminal(_ g: Galaxy) {
        CosmonBridge.openInTerminal(galaxyPath: g.path)
    }
}

struct GalaxiesView: View {
    @ObservedObject var model: GalaxiesViewModel
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Galaxies").font(.headline)

            if model.galaxies.isEmpty {
                Text("Aucune galaxy détectée sous `/srv/cosmon/*/`.")
                    .font(.footnote)
                    .foregroundColor(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                Spacer()
            } else {
                ScrollView {
                    VStack(alignment: .leading, spacing: 4) {
                        ForEach(model.galaxies) { g in
                            row(g)
                            Divider()
                        }
                    }
                    .padding(.top, 2)
                }
            }

            Text("Clique ⧉ pour ouvrir une Skylight (fenêtre whisper per-galaxie) ; ⌘⎈ pour ouvrir la galaxie en terminal. Session/whispers/inbox restent scopées à `/srv/cosmon/cosmon/` v1.")
                .font(.caption2)
                .foregroundColor(.secondary)
                .fixedSize(horizontal: false, vertical: true)
                .padding(.top, 4)
        }
        .padding(.horizontal, 12)
        .padding(.top, 4)
    }

    private func row(_ g: Galaxy) -> some View {
        HStack(spacing: 8) {
            VStack(alignment: .leading, spacing: 2) {
                Text(g.name)
                    .font(.footnote.weight(.medium))
                HStack(spacing: 6) {
                    if g.pendingCount > 0 {
                        Text("\(g.pendingCount) pending")
                            .font(.caption2)
                            .foregroundColor(.orange)
                    } else {
                        Text("0 pending")
                            .font(.caption2)
                            .foregroundColor(.secondary)
                    }
                    if let la = g.lastActivity {
                        Text("• \(Self.relativeTime(from: la))")
                            .font(.caption2)
                            .foregroundColor(.secondary)
                    }
                }
            }
            Spacer()
            Button {
                openWindow(value: g.path)
            } label: {
                Image(systemName: "rectangle.portrait.on.rectangle.portrait")
            }
            .buttonStyle(.borderless)
            .help("Ouvrir Skylight (whisper window) pour \(g.name)")
            Button {
                model.openInTerminal(g)
            } label: {
                Image(systemName: "terminal")
            }
            .buttonStyle(.borderless)
            .help("Ouvrir \(g.name) dans un terminal")
        }
        .padding(.vertical, 3)
    }

    private static func relativeTime(from date: Date) -> String {
        let seconds = Int(Date().timeIntervalSince(date))
        if seconds < 60 { return "à l'instant" }
        if seconds < 3600 { return "\(seconds / 60)m" }
        if seconds < 86_400 { return "\(seconds / 3600)h" }
        return "\(seconds / 86_400)j"
    }
}
