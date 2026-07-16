// SPDX-License-Identifier: MPL-2.0
//
// MoleculesScreen — molecule list for one galaxy. Tabbed status filter
// chips on top (running / pending / completed / collapsed / all),
// list rows underneath. Tap → MoleculeDetailScreen.

import SwiftUI
import CosmonAppKit

struct MoleculesScreen: View {
    let galaxy: String
    @EnvironmentObject var store: ClusterStore
    @State private var statusFilter: StatusFilter = .running

    enum StatusFilter: String, CaseIterable, Hashable {
        case running, pending, completed, collapsed, all

        var label: String {
            switch self {
            case .all: return "tous"
            default:   return rawValue
            }
        }

        var wireValue: String? {
            switch self {
            case .running:   return "running"
            case .pending:   return "pending"
            case .completed: return "completed"
            case .collapsed: return "collapsed"
            case .all:       return nil
            }
        }
    }

    var body: some View {
        VStack(spacing: 0) {
            FilterBar(selection: $statusFilter, onChange: { Task { await refresh() } })
                .padding(.horizontal)
                .padding(.top, 8)
                .padding(.bottom, 4)

            Divider()
                .background(CosmonPalette.charcoal.opacity(0.15))

            molecules
        }
        .navigationTitle(galaxy)
        .navigationBarTitleDisplayMode(.inline)
        .background(CosmonPalette.bone)
        .task { await refresh() }
    }

    @ViewBuilder
    private var molecules: some View {
        let mols = store.moleculesByGalaxy[galaxy] ?? []
        let filtered = mols.filter { row in
            guard let want = statusFilter.wireValue else { return true }
            return row.status == want
        }
        if filtered.isEmpty {
            EmptyMoleculesView(loading: store.moleculesLoading[galaxy] == true,
                                error: store.moleculesError[galaxy])
        } else {
            List(filtered) { mol in
                NavigationLink(value: MoleculeRoute(galaxy: galaxy, id: mol.id)) {
                    MoleculeRowView(molecule: mol)
                }
                .listRowBackground(CosmonPalette.bone)
            }
            .listStyle(.plain)
            .scrollContentBackground(.hidden)
            .background(CosmonPalette.bone)
            .refreshable { await refresh() }
            .navigationDestination(for: MoleculeRoute.self) { route in
                MoleculeDetailScreen(galaxy: route.galaxy, id: route.id)
            }
        }
    }

    private func refresh() async {
        await store.refreshMolecules(galaxy: galaxy, status: statusFilter.wireValue)
    }
}

struct MoleculeRoute: Hashable {
    let galaxy: String
    let id: String
}

private struct FilterBar: View {
    @Binding var selection: MoleculesScreen.StatusFilter
    var onChange: () -> Void

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                ForEach(MoleculesScreen.StatusFilter.allCases, id: \.self) { f in
                    Button {
                        selection = f
                        onChange()
                    } label: {
                        Text(f.label)
                            .font(.subheadline)
                            .padding(.horizontal, 12)
                            .padding(.vertical, 6)
                            .background(
                                Capsule()
                                    .fill(selection == f
                                          ? CosmonPalette.indigo.opacity(0.18)
                                          : CosmonPalette.charcoal.opacity(0.05))
                            )
                            .foregroundStyle(selection == f
                                             ? CosmonPalette.indigo
                                             : CosmonPalette.charcoal.opacity(0.7))
                    }
                    .buttonStyle(.plain)
                }
            }
        }
    }
}

private struct MoleculeRowView: View {
    let molecule: MoleculeSummary

    var body: some View {
        HStack(alignment: .center, spacing: 12) {
            Text(molecule.kindGlyph)
                .font(.title3)
                .frame(width: 30)
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Text(molecule.shortID)
                        .font(.body.monospaced())
                        .foregroundStyle(CosmonPalette.charcoal)
                    if molecule.totalSteps > 0 {
                        Text("\(molecule.currentStep + 1)/\(molecule.totalSteps)")
                            .font(.caption.monospacedDigit())
                            .foregroundStyle(CosmonPalette.charcoal.opacity(0.5))
                    }
                }
                Text(molecule.formula)
                    .font(.caption)
                    .foregroundStyle(CosmonPalette.charcoal.opacity(0.65))
                    .lineLimit(1)
            }
            Spacer()
            StatusBadge(status: molecule.status, liveness: molecule.liveness)
        }
        .padding(.vertical, 4)
    }
}

private struct StatusBadge: View {
    let status: String
    let liveness: String

    var body: some View {
        VStack(alignment: .trailing, spacing: 2) {
            Text(status)
                .font(.caption.weight(.medium))
                .foregroundStyle(CosmonPalette.status(status))
            if liveness == "zombie" {
                Text("zombie")
                    .font(.caption2)
                    .foregroundStyle(CosmonPalette.cadmium)
            }
        }
    }
}

private struct EmptyMoleculesView: View {
    let loading: Bool
    let error: String?

    var body: some View {
        VStack(spacing: 12) {
            if let error {
                Text("Galaxie injoignable")
                    .font(.headline)
                    .foregroundStyle(CosmonPalette.charcoal)
                Text(error)
                    .font(.footnote)
                    .foregroundStyle(CosmonPalette.charcoal.opacity(0.7))
                    .multilineTextAlignment(.center)
                    .padding(.horizontal)
            } else if loading {
                ProgressView()
            } else {
                Text("Aucune molécule")
                    .font(.headline)
                    .foregroundStyle(CosmonPalette.charcoal)
                Text("Rien à voir dans ce filtre.")
                    .font(.footnote)
                    .foregroundStyle(CosmonPalette.charcoal.opacity(0.6))
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(CosmonPalette.bone)
    }
}

#Preview {
    let store = ClusterStore(client: MockDaemonClient())
    return NavigationStack {
        MoleculesScreen(galaxy: "cosmon")
            .environmentObject(store)
    }
}
