import SwiftUI

@main
struct IOSPilotApp: App {
    @StateObject private var session = SessionStore()
    @StateObject private var settings = SettingsStore()

    var body: some Scene {
        WindowGroup {
            ContentView()
                .environmentObject(session)
                .environmentObject(settings)
                .task { await probeClusterOnFirstLaunch() }
        }
    }

    /// ADR-066: at first launch, probe `GET /cluster` on whatever cs-api
    /// we currently know about and, if the server returns a topology
    /// that pins `cs_api` to a specific host+port, adopt that URL in
    /// `UserDefaults`. When the server answers `not_configured` we
    /// keep the compile-time fallback — the pre-cluster.toml era still
    /// works. We only probe once per install: the marker defaults key
    /// `cs_api_cluster_probed` prevents re-overwriting an operator's
    /// manual edit.
    @MainActor
    private func probeClusterOnFirstLaunch() async {
        let defaults = UserDefaults.standard
        let markerKey = "cs_api_cluster_probed"
        guard !defaults.bool(forKey: markerKey) else { return }
        do {
            if let cluster = try await CosmonAPIFactory.shared.fetchCluster(),
               let resolved = cluster.csApiBaseURL {
                let url = resolved.absoluteString
                if url != settings.apiURL {
                    settings.apiURL = url
                }
            }
        } catch {
            // Transport or decode error — leave defaults untouched; the
            // operator can always set the URL by hand in Settings.
        }
        defaults.set(true, forKey: markerKey)
    }
}
