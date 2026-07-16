// SPDX-License-Identifier: MPL-2.0
//
// GalaxiesScreen — the first surface. One row per galaxy with a small
// icon (first letter), the galaxy name and the running/pending counts.
// Tap → drill into MoleculesScreen.

import SwiftUI
import CosmonAppKit

struct GalaxiesScreen: View {
    @EnvironmentObject var store: ClusterStore

    var body: some View {
        Group {
            if store.galaxies.isEmpty {
                EmptyClusterView(error: store.galaxiesError, loading: store.galaxiesLoading)
            } else {
                List(store.galaxies) { galaxy in
                    NavigationLink(value: galaxy) {
                        GalaxyRowView(galaxy: galaxy)
                    }
                    .listRowBackground(CosmonPalette.bone)
                }
                .listStyle(.plain)
                .scrollContentBackground(.hidden)
                .background(CosmonPalette.bone)
                .refreshable {
                    await store.refresh()
                }
            }
        }
        .navigationTitle("Galaxies")
        .navigationDestination(for: GalaxyRow.self) { galaxy in
            MoleculesScreen(galaxy: galaxy.name)
        }
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                if let h = store.health {
                    Text("\(h.galaxiesCount) gal · \(h.moleculesRunning) ▶")
                        .font(.caption.monospacedDigit())
                        .foregroundStyle(CosmonPalette.indigo)
                }
            }
        }
        .background(CosmonPalette.bone)
    }
}

private struct GalaxyRowView: View {
    let galaxy: GalaxyRow

    var body: some View {
        HStack(spacing: 14) {
            ZStack {
                Circle()
                    .fill(CosmonPalette.indigo.opacity(0.10))
                Text(initial(for: galaxy.name))
                    .font(.headline)
                    .foregroundStyle(CosmonPalette.indigo)
            }
            .frame(width: 36, height: 36)

            VStack(alignment: .leading, spacing: 2) {
                Text(galaxy.name)
                    .font(.body.weight(.medium))
                    .foregroundStyle(CosmonPalette.charcoal)
                Text(subtitle)
                    .font(.caption)
                    .foregroundStyle(CosmonPalette.charcoal.opacity(0.6))
            }

            Spacer()

            CountChip(value: galaxy.runningCount, label: "▶", color: CosmonPalette.indigo)
            if galaxy.pendingCount > 0 {
                CountChip(value: galaxy.pendingCount, label: "○", color: CosmonPalette.cadmium)
            }
        }
        .padding(.vertical, 4)
    }

    private var subtitle: String {
        "\(galaxy.moleculeCount) molécules"
    }

    private func initial(for name: String) -> String {
        guard let first = name.first else { return "?" }
        return String(first).uppercased()
    }
}

private struct CountChip: View {
    let value: Int
    let label: String
    let color: Color

    var body: some View {
        HStack(spacing: 2) {
            Text(label).font(.caption2)
            Text("\(value)").font(.caption.monospacedDigit())
        }
        .foregroundStyle(color)
        .padding(.horizontal, 8)
        .padding(.vertical, 3)
        .background(color.opacity(0.10), in: Capsule())
    }
}

private struct EmptyClusterView: View {
    let error: String?
    let loading: Bool

    var body: some View {
        VStack(spacing: 16) {
            if let error {
                Text("Connexion au daemon impossible")
                    .font(.headline)
                    .foregroundStyle(CosmonPalette.charcoal)
                Text(error)
                    .font(.callout)
                    .foregroundStyle(CosmonPalette.charcoal.opacity(0.7))
                    .multilineTextAlignment(.center)
                    .padding(.horizontal)
            } else if loading {
                ProgressView()
            } else {
                Text("Aucune galaxie")
                    .font(.headline)
                    .foregroundStyle(CosmonPalette.charcoal)
                Text("Le daemon ne voit rien dans /srv/cosmon/.")
                    .font(.callout)
                    .foregroundStyle(CosmonPalette.charcoal.opacity(0.7))
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(CosmonPalette.bone)
    }
}

#Preview {
    let store = ClusterStore(client: MockDaemonClient())
    return NavigationStack {
        GalaxiesScreen()
            .environmentObject(store)
            .task { await store.refreshGalaxies(); await store.refreshHealth() }
    }
}
