import SwiftUI

/// Store for the Motion pane — polls `GET /motion`.
@MainActor
public final class MotionStore: ObservableObject {
    @Published public private(set) var snapshot: MotionSnapshot = .empty
    @Published public private(set) var lastError: String?
    @Published public private(set) var isLoading: Bool = false

    private let api: CosmonAPIProtocol
    private var pollTask: Task<Void, Never>?

    public init(api: CosmonAPIProtocol = CosmonAPIFactory.shared) {
        self.api = api
    }

    deinit { pollTask?.cancel() }

    public func refresh() async {
        isLoading = true
        defer { isLoading = false }
        do {
            let fresh = try await api.motion(window: "15m")
            snapshot = fresh
            lastError = nil
        } catch {
            lastError = (error as? CosmonAPIError)?.errorDescription
                ?? error.localizedDescription
        }
    }

    public func startPolling(interval: TimeInterval) {
        pollTask?.cancel()
        pollTask = Task { [weak self] in
            let nanos = UInt64(max(1, interval) * 1_000_000_000)
            while !Task.isCancelled {
                await self?.refresh()
                try? await Task.sleep(nanoseconds: nanos)
            }
        }
    }

    public func stopPolling() {
        pollTask?.cancel()
        pollTask = nil
    }
}

struct MotionView: View {
    @EnvironmentObject var settings: SettingsStore
    @StateObject private var store: MotionStore

    init(store: MotionStore? = nil) {
        _store = StateObject(wrappedValue: store ?? MotionStore())
    }

    var body: some View {
        NavigationStack {
            List {
                headerSection
                workersSection
                moleculesSection
                commitsSection
                whispersSection
                sparksSection
                if let err = store.lastError {
                    Section {
                        Text(err)
                            .font(.caption)
                            .foregroundColor(.red)
                    }
                }
            }
            .listStyle(.insetGrouped)
            .refreshable { await store.refresh() }
            .navigationTitle("Motion")
            .navigationBarTitleDisplayMode(.inline)
        }
        .task {
            await store.refresh()
            if settings.pollingEnabled {
                store.startPolling(interval: max(3, settings.pollingInterval))
            }
        }
        .onDisappear { store.stopPolling() }
    }

    // MARK: - Sections

    private var headerSection: some View {
        Section {
            VStack(alignment: .leading, spacing: 4) {
                Text("\(store.snapshot.galaxiesScanned.count) galaxies · window \(store.snapshot.window.isEmpty ? "—" : store.snapshot.window)")
                    .font(.footnote)
                    .foregroundColor(.secondary)
                if !store.snapshot.galaxiesScanned.isEmpty {
                    Text(store.snapshot.galaxiesScanned.joined(separator: " · "))
                        .font(.caption2)
                        .foregroundColor(.secondary)
                }
            }
        }
    }

    private var workersSection: some View {
        Section("Workers (\(store.snapshot.workers.count))") {
            if store.snapshot.workers.isEmpty {
                Text("—").foregroundColor(.secondary)
            } else {
                ForEach(store.snapshot.workers) { w in
                    WorkerRow(worker: w)
                }
            }
        }
    }

    private var moleculesSection: some View {
        Section("Running (\(store.snapshot.runningMolecules.count))") {
            if store.snapshot.runningMolecules.isEmpty {
                Text("—").foregroundColor(.secondary)
            } else {
                ForEach(store.snapshot.runningMolecules) { m in
                    MoleculeRow(molecule: m)
                }
            }
        }
    }

    private var commitsSection: some View {
        Section("Commits (\(store.snapshot.recentCommits.count))") {
            if store.snapshot.recentCommits.isEmpty {
                Text("—").foregroundColor(.secondary)
            } else {
                ForEach(store.snapshot.recentCommits) { c in
                    CommitRow(commit: c)
                }
            }
        }
    }

    private var whispersSection: some View {
        Section("Whispers (\(store.snapshot.recentWhispers.count))") {
            if store.snapshot.recentWhispers.isEmpty {
                Text("—").foregroundColor(.secondary)
            } else {
                ForEach(store.snapshot.recentWhispers) { w in
                    WhisperRowInline(whisper: w)
                }
            }
        }
    }

    private var sparksSection: some View {
        Section("Sparks (\(store.snapshot.recentSparks.count))") {
            if store.snapshot.recentSparks.isEmpty {
                Text("—").foregroundColor(.secondary)
            } else {
                ForEach(store.snapshot.recentSparks) { s in
                    SparkRow(spark: s)
                }
            }
        }
    }
}

// MARK: - Rows

private struct WorkerRow: View {
    let worker: MotionWorker

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            Circle()
                .fill(statusColor)
                .frame(width: 8, height: 8)
                .padding(.top, 6)
            VStack(alignment: .leading, spacing: 2) {
                Text(worker.name)
                    .font(.subheadline.monospaced())
                    .lineLimit(1)
                HStack(spacing: 4) {
                    Text(worker.galaxy)
                        .font(.caption2)
                        .foregroundColor(.secondary)
                    if let mol = worker.moleculeID {
                        Text("·").foregroundColor(.secondary)
                        Text(mol)
                            .font(.caption2)
                            .foregroundColor(.accentColor)
                            .lineLimit(1)
                    }
                }
                if let hb = worker.lastHeartbeat {
                    Text(formatTime(hb))
                        .font(.caption2.monospaced())
                        .foregroundColor(.secondary)
                }
            }
        }
    }

    private var statusColor: Color {
        switch worker.status?.lowercased() {
        case "active", "healthy", "running": return .green
        case "stopped", "stale": return .yellow
        case "error", "diverged": return .red
        default: return .gray
        }
    }
}

private struct MoleculeRow: View {
    let molecule: MotionMolecule

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 6) {
                Text(molecule.id)
                    .font(.subheadline.monospaced())
                Spacer()
                Text(molecule.stepLabel)
                    .font(.caption)
                    .foregroundColor(.orange)
            }
            HStack(spacing: 4) {
                Text(molecule.galaxy)
                    .font(.caption2)
                    .foregroundColor(.secondary)
                if let ev = molecule.lastEvolveAt {
                    Text("·").foregroundColor(.secondary)
                    Text(formatTime(ev))
                        .font(.caption2.monospaced())
                        .foregroundColor(.secondary)
                }
            }
            if let topic = molecule.topicPreview, !topic.isEmpty {
                Text(topic)
                    .font(.caption)
                    .foregroundColor(.secondary)
                    .lineLimit(2)
            }
        }
    }
}

private struct CommitRow: View {
    let commit: MotionCommit

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 6) {
                Text(commit.sha)
                    .font(.caption.monospaced())
                    .foregroundColor(.orange)
                Text(commit.galaxy)
                    .font(.caption2)
                    .foregroundColor(.secondary)
                Spacer()
                Text(formatTime(commit.timestamp))
                    .font(.caption2.monospaced())
                    .foregroundColor(.secondary)
            }
            Text(commit.subject)
                .font(.subheadline)
                .lineLimit(2)
            if !commit.author.isEmpty {
                Text(commit.author)
                    .font(.caption2)
                    .foregroundColor(.secondary)
            }
        }
    }
}

private struct WhisperRowInline: View {
    let whisper: MotionWhisper

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 6) {
                Text(whisper.galaxy).font(.caption2).foregroundColor(.secondary)
                if let sender = whisper.senderNucleonID {
                    Text(sender)
                        .font(.caption2)
                        .foregroundColor(.accentColor)
                }
                Spacer()
                Text(formatTime(whisper.receivedAt))
                    .font(.caption2.monospaced())
                    .foregroundColor(.secondary)
            }
            Text(whisper.bodyPreview)
                .font(.subheadline)
                .lineLimit(3)
        }
    }
}

private struct SparkRow: View {
    let spark: MotionSpark

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 6) {
                Text(spark.id)
                    .font(.subheadline.monospaced())
                Spacer()
                Text(formatTime(spark.createdAt))
                    .font(.caption2.monospaced())
                    .foregroundColor(.secondary)
            }
            Text(spark.galaxy)
                .font(.caption2)
                .foregroundColor(.secondary)
            if let topic = spark.topicPreview, !topic.isEmpty {
                Text(topic)
                    .font(.caption)
                    .foregroundColor(.secondary)
                    .lineLimit(2)
            }
        }
    }
}

// MARK: - Helpers

private let motionTimeFormatter: DateFormatter = {
    let f = DateFormatter()
    f.dateFormat = "HH:mm:ss"
    return f
}()

private func formatTime(_ iso8601: String) -> String {
    if iso8601.isEmpty { return "-" }
    let iso = ISO8601DateFormatter()
    iso.formatOptions = [.withInternetDateTime]
    if let d = iso.date(from: iso8601) {
        return motionTimeFormatter.string(from: d)
    }
    let relaxed = ISO8601DateFormatter()
    relaxed.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
    if let d = relaxed.date(from: iso8601) {
        return motionTimeFormatter.string(from: d)
    }
    return iso8601
}
