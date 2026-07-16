//
//  ClusterView.swift
//  ios-pilot
//
//  Cluster-wide iOS view: one tab with a segmented sub-picker toggling
//  between "Ensemble" (workers + molecule groups per galaxy) and "Peek"
//  (monospaced fractal snapshot at city / building / skin scales).
//  Both panels call cs-api `/ensemble` and `/peek` directly through
//  the shared CosmonAPI client.
//

import SwiftUI
import Foundation

enum ClusterPane: String, CaseIterable, Identifiable {
    case ensemble = "Ensemble"
    case peek     = "Peek"
    var id: String { rawValue }
}

@MainActor
final class ClusterEnsembleStore: ObservableObject {
    @Published private(set) var response: ClusterEnsembleResponse?
    @Published private(set) var lastError: String?

    private let api: CosmonAPIProtocol
    private var timerTask: Task<Void, Never>?

    init(api: CosmonAPIProtocol) {
        self.api = api
    }

    func startPolling(every seconds: UInt64 = 10) {
        stopPolling()
        let api = self.api
        timerTask = Task { @MainActor [weak self] in
            while !Task.isCancelled {
                do {
                    let r = try await api.ensemble(scope: "local")
                    self?.response = r
                    self?.lastError = nil
                } catch {
                    self?.lastError = (error as? LocalizedError)?.errorDescription
                        ?? error.localizedDescription
                }
                try? await Task.sleep(nanoseconds: seconds * 1_000_000_000)
            }
        }
    }

    func stopPolling() {
        timerTask?.cancel()
        timerTask = nil
    }

    func refresh() async {
        do {
            response = try await api.ensemble(scope: "local")
            lastError = nil
        } catch {
            lastError = (error as? LocalizedError)?.errorDescription
                ?? error.localizedDescription
        }
    }
}

@MainActor
final class ClusterPeekStore: ObservableObject {
    @Published var scale: String = "building"
    @Published var focus: String = ""
    @Published private(set) var text: String = ""
    @Published private(set) var lastError: String?

    private let api: CosmonAPIProtocol

    init(api: CosmonAPIProtocol) {
        self.api = api
    }

    func refresh() async {
        do {
            let f = focus.trimmingCharacters(in: .whitespaces)
            let resp = try await api.peek(scale: scale, focus: f.isEmpty ? nil : f)
            text = resp.text
            lastError = nil
        } catch {
            lastError = (error as? LocalizedError)?.errorDescription
                ?? error.localizedDescription
        }
    }
}

struct ClusterView: View {
    @StateObject private var ensembleStore: ClusterEnsembleStore
    @StateObject private var peekStore: ClusterPeekStore
    @State private var pane: ClusterPane = .ensemble

    init(api: CosmonAPIProtocol = CosmonAPIFactory.shared) {
        _ensembleStore = StateObject(wrappedValue: ClusterEnsembleStore(api: api))
        _peekStore = StateObject(wrappedValue: ClusterPeekStore(api: api))
    }

    var body: some View {
        NavigationStack {
            VStack(spacing: 0) {
                Picker("Vue", selection: $pane) {
                    ForEach(ClusterPane.allCases) { p in
                        Text(p.rawValue).tag(p)
                    }
                }
                .pickerStyle(.segmented)
                .padding(.horizontal)
                .padding(.top, 6)

                Divider()

                switch pane {
                case .ensemble: ClusterEnsemblePane(store: ensembleStore)
                case .peek:     ClusterPeekPane(store: peekStore)
                }
            }
            .navigationTitle("Cluster")
        }
        .task {
            await ensembleStore.refresh()
            ensembleStore.startPolling()
            await peekStore.refresh()
        }
        .onDisappear { ensembleStore.stopPolling() }
    }
}

private struct ClusterEnsemblePane: View {
    @ObservedObject var store: ClusterEnsembleStore

    var body: some View {
        List {
            if let err = store.lastError, store.response == nil {
                Section {
                    Text(err).foregroundColor(.red)
                }
            }
            if let r = store.response {
                Section(header: Text("Cluster")) {
                    HStack {
                        Label("\(r.totals.galaxies) galaxies", systemImage: "circles.hexagongrid")
                        Spacer()
                        Text("\(r.totals.workers) w · \(r.totals.molecules) m")
                            .font(.caption.monospaced())
                            .foregroundColor(.secondary)
                    }
                }
                ForEach(r.galaxies) { g in
                    Section(header:
                        HStack {
                            Text(g.name).font(.headline)
                            Spacer()
                            Text("\(g.workerCount) w · \(g.totalMolecules) m")
                                .font(.caption.monospaced())
                                .foregroundColor(.secondary)
                        }
                    ) {
                        if g.workers.isEmpty && g.moleculeGroups.isEmpty {
                            Text("(vide)").foregroundColor(.secondary).font(.caption)
                        }
                        ForEach(g.moleculeGroups) { grp in
                            HStack {
                                Text(grp.status)
                                    .font(.caption.monospaced())
                                    .frame(width: 90, alignment: .leading)
                                    .foregroundColor(.secondary)
                                Text("\(grp.total)")
                                    .font(.caption.monospaced().bold())
                                Spacer()
                            }
                        }
                        ForEach(g.workers) { w in
                            HStack(spacing: 6) {
                                Image(systemName: w.live ? "circle.fill" : "circle")
                                    .foregroundColor(w.live ? .green : .secondary)
                                    .font(.caption2)
                                Text(w.name).font(.caption)
                                Spacer()
                                if let m = w.moleculeId {
                                    Text(m).font(.caption2.monospaced()).foregroundColor(.secondary)
                                }
                            }
                        }
                    }
                }
            } else if store.lastError == nil {
                ProgressView().frame(maxWidth: .infinity)
            }
        }
        .listStyle(.insetGrouped)
    }
}

private struct ClusterPeekPane: View {
    @ObservedObject var store: ClusterPeekStore

    var body: some View {
        VStack(spacing: 8) {
            Picker("Scale", selection: $store.scale) {
                Text("City").tag("city")
                Text("Building").tag("building")
                Text("Skin").tag("skin")
            }
            .pickerStyle(.segmented)
            .padding(.horizontal)
            .onChange(of: store.scale) { _, _ in
                Task { await store.refresh() }
            }

            TextField("focus (galaxy ou molecule id)", text: $store.focus)
                .textFieldStyle(.roundedBorder)
                .padding(.horizontal)
                .onSubmit { Task { await store.refresh() } }

            if let err = store.lastError, store.text.isEmpty {
                Text(err).font(.footnote).foregroundColor(.red)
            }

            ScrollView([.vertical, .horizontal]) {
                Text(store.text.isEmpty ? "Chargement…" : store.text)
                    .font(.system(.caption2, design: .monospaced))
                    .textSelection(.enabled)
                    .padding(10)
            }
        }
        .refreshable { await store.refresh() }
    }
}

#Preview {
    ClusterView(api: MockCosmonAPI())
}
