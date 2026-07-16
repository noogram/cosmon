// SPDX-License-Identifier: MPL-2.0
//
// ContentView — three-tab root. Galaxies on the left, Fleets on the
// right, daemon-offline / saturated-silence banner on top. The detail
// drill-down is rooted in NavigationStack inside each tab so the URL-
// like back-stack persists across the polling refreshes.

import SwiftUI
import CosmonAppKit

struct ContentView: View {
    @EnvironmentObject var store: ClusterStore

    var body: some View {
        ZStack(alignment: .bottom) {
            CosmonPalette.bone.ignoresSafeArea()
            TabView {
                NavigationStack {
                    GalaxiesScreen()
                }
                .tabItem {
                    Label("Galaxies", systemImage: "circle.grid.2x2")
                }

                NavigationStack {
                    FleetsScreen()
                }
                .tabItem {
                    Label("Fleets", systemImage: "person.3")
                }
            }
            .tint(CosmonPalette.indigo)

            if !store.hasReachedDaemon, let err = store.lastError {
                DaemonOfflineBanner(message: err)
                    .transition(.move(edge: .bottom).combined(with: .opacity))
            }
        }
    }
}

private struct DaemonOfflineBanner: View {
    let message: String
    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text("Daemon hors ligne")
                .font(.headline)
                .foregroundStyle(CosmonPalette.charcoal)
            Text(message)
                .font(.footnote)
                .foregroundStyle(CosmonPalette.charcoal.opacity(0.7))
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(CosmonPalette.cadmium.opacity(0.18))
        .overlay(alignment: .top) {
            Rectangle()
                .fill(CosmonPalette.cadmium)
                .frame(height: 2)
        }
    }
}
