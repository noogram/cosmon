import SwiftUI

/// Shared stores for the three v1 tabs — lifted to ContentView so the
/// tab badges can reflect live counts without per-view refetches.
@MainActor
final class PilotStores: ObservableObject {
    let whispers: WhispersStore
    let inbox: InboxStore
    let galaxies: GalaxiesStore

    init(api: CosmonAPIProtocol = CosmonAPIFactory.shared) {
        self.whispers = WhispersStore(api: api)
        self.inbox = InboxStore(api: api)
        self.galaxies = GalaxiesStore(api: api)
    }
}

struct ContentView: View {
    @EnvironmentObject var settings: SettingsStore
    @StateObject private var stores = PilotStores()

    var body: some View {
        TabView {
            SessionView()
                .tabItem {
                    Label("Session", systemImage: "square.and.pencil")
                }

            WhispersView(store: stores.whispers)
                .tabItem {
                    Label("Whispers", systemImage: "bubble.left.and.bubble.right")
                }
                .badge(stores.whispers.unreadCount)

            InboxView(store: stores.inbox)
                .tabItem {
                    Label("Inbox", systemImage: "tray")
                }
                .badge(stores.inbox.hotCount)

            GalaxiesView(store: stores.galaxies)
                .tabItem {
                    Label("Galaxies", systemImage: "circles.hexagongrid")
                }

            ClusterView()
                .tabItem {
                    Label("Cluster", systemImage: "square.grid.3x3.square")
                }

            SettingsView()
                .tabItem {
                    Label("Réglages", systemImage: "gearshape")
                }
        }
    }
}

#Preview {
    ContentView()
        .environmentObject(SessionStore(api: MockCosmonAPI()))
        .environmentObject(SettingsStore())
}
