// SPDX-License-Identifier: MPL-2.0
//
// FleetsScreen — one row per galaxy fleet. Worker count is the only
// number that matters today. Repos count and attention budget are
// surfaced for diagnosis.

import SwiftUI
import CosmonAppKit

struct FleetsScreen: View {
    @EnvironmentObject var store: ClusterStore

    var body: some View {
        Group {
            if store.fleets.isEmpty {
                VStack(spacing: 12) {
                    Text("Aucune fleet")
                        .font(.headline)
                        .foregroundStyle(CosmonPalette.charcoal)
                    Text("Le daemon n'a vu aucun /srv/cosmon/<g>/.cosmon/state/.")
                        .font(.callout)
                        .foregroundStyle(CosmonPalette.charcoal.opacity(0.7))
                        .multilineTextAlignment(.center)
                        .padding(.horizontal)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .background(CosmonPalette.bone)
            } else {
                List(store.fleets) { fleet in
                    FleetRowView(fleet: fleet)
                        .listRowBackground(CosmonPalette.bone)
                }
                .listStyle(.plain)
                .scrollContentBackground(.hidden)
                .background(CosmonPalette.bone)
                .refreshable { await store.refreshFleets() }
            }
        }
        .navigationTitle("Fleets")
        .background(CosmonPalette.bone)
        .task { await store.refreshFleets() }
    }
}

private struct FleetRowView: View {
    let fleet: FleetRow

    var body: some View {
        HStack(spacing: 12) {
            VStack(alignment: .leading, spacing: 2) {
                Text(fleet.galaxy)
                    .font(.body.weight(.medium))
                    .foregroundStyle(CosmonPalette.charcoal)
                if let budget = fleet.attentionBudget {
                    Text("budget \(budget)")
                        .font(.caption)
                        .foregroundStyle(CosmonPalette.charcoal.opacity(0.6))
                }
            }
            Spacer()
            VStack(alignment: .trailing, spacing: 2) {
                Text("\(fleet.workerCount) workers")
                    .font(.subheadline.monospacedDigit())
                    .foregroundStyle(CosmonPalette.indigo)
                if fleet.repoCount > 0 {
                    Text("\(fleet.repoCount) repos")
                        .font(.caption.monospacedDigit())
                        .foregroundStyle(CosmonPalette.charcoal.opacity(0.5))
                }
            }
        }
        .padding(.vertical, 4)
    }
}

#Preview {
    let store = ClusterStore(client: MockDaemonClient())
    return NavigationStack {
        FleetsScreen()
            .environmentObject(store)
            .task { await store.refreshFleets() }
    }
}
