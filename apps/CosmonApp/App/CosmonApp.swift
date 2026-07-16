// SPDX-License-Identifier: MPL-2.0
//
// Entry point — universal iPhone+iPad target. CosmonApp is the third
// native app of the local cluster (after Verdict and Mur du Matin) and
// re-uses the same trust boundary (Tailscale) and the same wire
// foundation (`AppsTransportHTTP`).
//
// On launch the store starts the polling loop (5 s); on background it
// stops to keep the radio quiet — cosmon state moves slowly and there
// is no urgency to refresh while the operator is in another app.

import SwiftUI
import CosmonAppKit

@main
struct CosmonApp: App {
    @StateObject private var store: ClusterStore = {
        // Production builds always hit the live cosmon-daemon. DEBUG
        // builds honour `COSMON_USE_MOCK=1` for simulator iteration
        // without a Mac on the tailnet.
        #if DEBUG
        if ProcessInfo.processInfo.environment["COSMON_USE_MOCK"] == "1" {
            return ClusterStore(client: MockDaemonClient())
        }
        #endif
        return ClusterStore(client: LiveDaemonClient.fromInfoPlist())
    }()

    @Environment(\.scenePhase) private var scenePhase

    var body: some Scene {
        WindowGroup("Cosmon") {
            ContentView()
                .environmentObject(store)
                .task {
                    store.startPolling()
                }
        }
        .onChange(of: scenePhase) { _, phase in
            switch phase {
            case .active:
                store.startPolling()
            case .background, .inactive:
                store.stopPolling()
            @unknown default:
                break
            }
        }
    }
}
