//
//  CosmonBridge.swift
//  mac-pilot
//
//  Bridge between the SwiftUI popover and the `cs` CLI. v0 is a thin
//  shell-out: we spawn `/path/to/cs session <verb>` via `Process` and map
//  exit codes 2 (`session already open`) and 3 (`no open session`) to typed
//  `CosmonError` cases.
//
//  The `cs` CLI does not yet expose a `session current` subcommand, so we
//  read the sessions directory directly and recognise the open session as
//  the one without a closing `---` frontmatter block. This parser mirrors
//  the exact file layout produced by `crates/cosmon-cli/src/cmd/session.rs`
//  (frontmatter / `## HH:MM:SS — tag` note headings / optional sealed
//  footer).
//

import Foundation

enum CosmonBridge {

    // MARK: - Configuration

    /// Galaxy root. v1 still hardcoded to `/srv/cosmon/cosmon` — every pane
    /// (session, whispers, inbox) reads from here; the Galaxies pane only
    /// lists peers and opens them in a terminal. A true multi-galaxy picker
    /// is deferred to v2.
    static let galaxyRoot: URL = {
        let home = FileManager.default.homeDirectoryForCurrentUser
        return home.appendingPathComponent("galaxies/cosmon", isDirectory: true)
    }()

    /// Directory where `cs session start` lays down `session-*.md` files.
    static var sessionsDir: URL {
        galaxyRoot.appendingPathComponent(".cosmon/state/sessions", isDirectory: true)
    }

    /// Parent directory holding every sibling galaxy (`~/galaxies`).
    static let galaxiesRoot: URL = {
        let home = FileManager.default.homeDirectoryForCurrentUser
        return home.appendingPathComponent("galaxies", isDirectory: true)
    }()

    /// Directory where ingress deposits incoming whispers, organised by room.
    static var whispersInboxRoot: URL {
        galaxyRoot.appendingPathComponent(".cosmon/whispers/inbox", isDirectory: true)
    }

    /// Archive destination when a whisper is marked as read.
    static var whispersArchiveRoot: URL {
        galaxyRoot.appendingPathComponent(".cosmon/whispers/archived", isDirectory: true)
    }

    /// Sidecar directory recording which session notes have already been
    /// promoted into `spark` molecules (mirrors what
    /// `scripts/session-to-spark-tick.sh` writes). Presence of a file
    /// under `<session_id>/<HH-MM-SS>.md` means the note has been
    /// promoted already — the UI reads this to hide the "Promouvoir en
    /// spark" button from notes that are already done.
    static var sessionPromotedRoot: URL {
        galaxyRoot.appendingPathComponent(".cosmon/state/sessions/.promoted", isDirectory: true)
    }

    // MARK: - cs binary resolution

    private static var cachedPath: String?
    private static let pathLock = NSLock()

    /// Resolve the `cs` binary path, in order of preference:
    /// 1. `CS_BINARY_PATH` environment variable (set from Xcode scheme).
    /// 2. `$HOME/.local/bin/cs` (default install location).
    /// 3. `/usr/bin/env which cs` (PATH fallback).
    static func resolveCsPath() throws -> String {
        pathLock.lock()
        defer { pathLock.unlock() }
        if let cached = cachedPath { return cached }

        let env = ProcessInfo.processInfo.environment
        if let override = env["CS_BINARY_PATH"], !override.isEmpty,
           FileManager.default.isExecutableFile(atPath: override) {
            cachedPath = override
            return override
        }

        let home = FileManager.default.homeDirectoryForCurrentUser.path
        let localBin = "\(home)/.local/bin/cs"
        if FileManager.default.isExecutableFile(atPath: localBin) {
            cachedPath = localBin
            return localBin
        }

        let task = Process()
        task.executableURL = URL(fileURLWithPath: "/usr/bin/env")
        task.arguments = ["which", "cs"]
        let pipe = Pipe()
        task.standardOutput = pipe
        task.standardError = Pipe()
        do {
            try task.run()
        } catch {
            throw CosmonError.csNotFound
        }
        task.waitUntilExit()
        if task.terminationStatus == 0 {
            let data = pipe.fileHandleForReading.readDataToEndOfFile()
            if let raw = String(data: data, encoding: .utf8)?
                .trimmingCharacters(in: .whitespacesAndNewlines),
               !raw.isEmpty,
               FileManager.default.isExecutableFile(atPath: raw) {
                cachedPath = raw
                return raw
            }
        }
        throw CosmonError.csNotFound
    }

    /// Reset the cached `cs` binary location (useful after the operator updates
    /// `CS_BINARY_PATH` in the Xcode scheme without relaunching).
    static func resetCache() {
        pathLock.lock()
        cachedPath = nil
        pathLock.unlock()
    }

    // MARK: - Shell-out helpers

    private struct RunResult {
        let stdout: String
        let stderr: String
        let status: Int32
    }

    private static func run(_ args: [String]) async throws -> RunResult {
        try await runAt(cwd: galaxyRoot, args: args)
    }

    /// Shell out to `cs` with an explicit working directory, so per-galaxy
    /// operations (Skylight whisper feed, emit-to-molecule) reach the right
    /// `.cosmon/` via walk-up discovery.
    private static func runAt(cwd: URL, args: [String]) async throws -> RunResult {
        let path = try resolveCsPath()
        return try await Task.detached(priority: .userInitiated) { () throws -> RunResult in
            let task = Process()
            task.executableURL = URL(fileURLWithPath: path)
            task.arguments = args
            task.currentDirectoryURL = cwd

            // Forward a minimal environment — keep `HOME` and `PATH` so that
            // walk-up discovery of `.cosmon/` and subprocess spawns still work.
            var env = ProcessInfo.processInfo.environment
            if env["PATH"] == nil {
                env["PATH"] = "/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin"
            }
            task.environment = env

            let outPipe = Pipe()
            let errPipe = Pipe()
            task.standardOutput = outPipe
            task.standardError = errPipe
            try task.run()
            task.waitUntilExit()
            let outData = outPipe.fileHandleForReading.readDataToEndOfFile()
            let errData = errPipe.fileHandleForReading.readDataToEndOfFile()
            let stdout = String(data: outData, encoding: .utf8) ?? ""
            let stderr = String(data: errData, encoding: .utf8) ?? ""
            return RunResult(stdout: stdout, stderr: stderr, status: task.terminationStatus)
        }.value
    }

    // MARK: - Public API

    /// Start a new session. Returns the `SessionID` of the freshly opened carnet.
    @discardableResult
    static func start(galaxy: String?) async throws -> SessionID {
        var args = ["session", "start"]
        if let galaxy, !galaxy.isEmpty {
            args.append(contentsOf: ["--galaxy", galaxy])
        }
        let result = try await run(args)
        if result.status == 2 {
            let path = extractPath(from: result.stderr.isEmpty ? result.stdout : result.stderr)
            throw CosmonError.sessionAlreadyOpen(path: path)
        }
        guard result.status == 0 else {
            throw CosmonError.executionFailed(
                exitCode: result.status,
                stderr: result.stderr.isEmpty ? result.stdout : result.stderr
            )
        }
        if let state = try await current() { return state.sessionID }
        throw CosmonError.parseFailure("session start succeeded but no open session detected")
    }

    /// Append a note to the currently open session.
    static func note(_ text: String, tag: String?) async throws {
        let trimmedText = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmedText.isEmpty else {
            throw CosmonError.parseFailure("empty note body")
        }
        var args = ["session", "note"]
        if let tag, !tag.trimmingCharacters(in: .whitespaces).isEmpty {
            args.append(contentsOf: ["--tag", tag.trimmingCharacters(in: .whitespaces)])
        }
        args.append(trimmedText)
        let result = try await run(args)
        if result.status == 3 { throw CosmonError.noSessionOpen }
        guard result.status == 0 else {
            throw CosmonError.executionFailed(
                exitCode: result.status,
                stderr: result.stderr.isEmpty ? result.stdout : result.stderr
            )
        }
    }

    /// End the current session. Returns the `Seal` recovered from the sealed file footer.
    static func end() async throws -> Seal {
        let stateBefore = try await current()
        let result = try await run(["session", "end"])
        if result.status == 3 { throw CosmonError.noSessionOpen }
        guard result.status == 0 else {
            throw CosmonError.executionFailed(
                exitCode: result.status,
                stderr: result.stderr.isEmpty ? result.stdout : result.stderr
            )
        }
        let id = stateBefore?.sessionID ?? SessionID(raw: "")
        let hash = extractSealHash(for: id) ?? ""
        return Seal(sessionID: id, hash: hash)
    }

    /// Returns the state of the currently open session, or `nil` if none.
    ///
    /// Implementation note: `cs session current` does not exist. We instead
    /// walk `.cosmon/state/sessions/` and pick the single file that does not
    /// yet carry a closing `---` frontmatter footer. If several candidates
    /// exist (should never happen) we take the lexicographically largest one,
    /// i.e. the most recent.
    static func current() async throws -> SessionState? {
        let fm = FileManager.default
        guard fm.fileExists(atPath: sessionsDir.path) else { return nil }
        let children = (try? fm.contentsOfDirectory(at: sessionsDir, includingPropertiesForKeys: nil)) ?? []
        let candidates = children
            .filter { $0.lastPathComponent.hasPrefix("session-") && $0.pathExtension == "md" }
            .sorted { $0.lastPathComponent > $1.lastPathComponent }
        for url in candidates {
            guard let body = try? String(contentsOf: url, encoding: .utf8) else { continue }
            if let state = SessionParser.parseOpen(body: body) {
                return state
            }
        }
        return nil
    }

    /// Promote a single session note into a `spark` molecule by shelling
    /// out to `cs session promote <note_ts> --session <sid>`.
    ///
    /// The CLI wrapper handles the open-session detection and the
    /// sidecar bookkeeping — this function only needs to translate the
    /// parsed `Note` into a `<ts>` argument. Returns the newly created
    /// spark molecule id parsed from the tick's NDJSON, or empty string
    /// if the id cannot be recovered (nucleation still succeeded).
    @discardableResult
    static func promoteSessionNote(sessionID: SessionID, note: Note) async throws -> String {
        let ts = note.timestamp
        guard !ts.isEmpty else {
            throw CosmonError.parseFailure("note has no timestamp")
        }
        let result = try await run([
            "--json",
            "session", "promote",
            "--session", sessionID.raw,
            ts,
        ])
        guard result.status == 0 else {
            throw CosmonError.executionFailed(
                exitCode: result.status,
                stderr: result.stderr.isEmpty ? result.stdout : result.stderr
            )
        }
        // The tick emits one NDJSON line per processed note plus a
        // trailing `tick_complete`. Scan for the first `spark_created`
        // record for this note_ts and return its spark_id.
        for raw in result.stdout.components(separatedBy: "\n") {
            let line = raw.trimmingCharacters(in: .whitespaces)
            guard line.hasPrefix("{"), line.contains("\"spark_created\"") else { continue }
            guard let data = line.data(using: .utf8),
                  let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
                  let sparkID = obj["spark_id"] as? String,
                  let noteTs = obj["note_ts"] as? String,
                  noteTs == ts else {
                continue
            }
            return sparkID
        }
        return ""
    }

    /// Set of `<session_id>/<HH-MM-SS>.md` sidecar paths indicating
    /// notes that have already been promoted into sparks.
    ///
    /// The UI uses this to disable the "Promouvoir en spark" button on
    /// a note row that already has a sidecar. Safe to call from a
    /// background Task — reads are cheap (one readdir + small files),
    /// and a missing root directory is treated as "nothing promoted".
    static func promotedNoteTimestamps(sessionID: SessionID) -> Set<String> {
        let fm = FileManager.default
        let dir = sessionPromotedRoot.appendingPathComponent(sessionID.raw, isDirectory: true)
        guard fm.fileExists(atPath: dir.path) else { return [] }
        let children = (try? fm.contentsOfDirectory(at: dir, includingPropertiesForKeys: nil)) ?? []
        var result: Set<String> = []
        for url in children where url.pathExtension.lowercased() == "md" {
            // Filename is `<HH-MM-SS>.md` (colons replaced for FS safety).
            // Reconstruct the `HH:MM:SS` form so callers can compare against
            // a parsed `Note.timestamp`.
            let stem = url.deletingPathExtension().lastPathComponent
            let parts = stem.split(separator: "-")
            if parts.count == 3 {
                result.insert("\(parts[0]):\(parts[1]):\(parts[2])")
            }
        }
        return result
    }

    // MARK: - Helpers

    private static func extractSealHash(for id: SessionID) -> String? {
        guard !id.raw.isEmpty else { return nil }
        let url = sessionsDir.appendingPathComponent("\(id.raw).md")
        guard let body = try? String(contentsOf: url, encoding: .utf8) else { return nil }
        for raw in body.components(separatedBy: "\n") {
            let line = raw.trimmingCharacters(in: .whitespaces)
            if line.hasPrefix("seal:") {
                return String(line.dropFirst("seal:".count)).trimmingCharacters(in: .whitespaces)
            }
        }
        return nil
    }

    private static func extractPath(from stderr: String) -> String? {
        // Error shape: "a session is already open: /path/to/session-*.md — close it first..."
        let scanner = Scanner(string: stderr)
        scanner.charactersToBeSkipped = nil
        _ = scanner.scanUpToString(": ")
        guard scanner.scanString(": ") != nil else { return nil }
        let tail = scanner.scanUpToString(" —") ?? scanner.scanUpToString("\n")
        return tail?.trimmingCharacters(in: .whitespaces)
    }

    // MARK: - Whispers

    /// Enumerate every whisper `.md` file under `inboxRoot`.
    ///
    /// Returns an empty list if the directory is missing — that is the normal
    /// state for a fresh install. Sorted newest-first by `receivedAt`.
    static func listWhispers(inboxRoot: URL = whispersInboxRoot) async throws -> [Whisper] {
        try await Task.detached(priority: .userInitiated) { () -> [Whisper] in
            let fm = FileManager.default
            guard fm.fileExists(atPath: inboxRoot.path) else { return [] }
            let rooms = (try? fm.contentsOfDirectory(
                at: inboxRoot,
                includingPropertiesForKeys: [.isDirectoryKey],
                options: [.skipsHiddenFiles]
            )) ?? []
            var out: [Whisper] = []
            for roomDir in rooms {
                var isDir: ObjCBool = false
                guard fm.fileExists(atPath: roomDir.path, isDirectory: &isDir), isDir.boolValue else {
                    continue
                }
                let files = (try? fm.contentsOfDirectory(
                    at: roomDir,
                    includingPropertiesForKeys: [.contentModificationDateKey],
                    options: [.skipsHiddenFiles]
                )) ?? []
                for url in files where url.pathExtension == "md" {
                    guard let body = try? String(contentsOf: url, encoding: .utf8) else { continue }
                    if let w = WhisperParser.parse(
                        body: body,
                        url: url,
                        roomDirectoryName: roomDir.lastPathComponent
                    ) {
                        out.append(w)
                    }
                }
            }
            return out.sorted { $0.receivedAt > $1.receivedAt }
        }.value
    }

    /// Move a whisper file from `inbox/<room>/` to `archived/<room>/` with the
    /// same filename. Creates the destination room directory as needed.
    /// Idempotent — missing source is treated as already-archived.
    static func archiveWhisper(_ whisper: Whisper) async throws {
        try await Task.detached(priority: .userInitiated) { () throws -> Void in
            let fm = FileManager.default
            let archiveDir = whispersArchiveRoot
                .appendingPathComponent(whisper.roomDirectoryName, isDirectory: true)
            try fm.createDirectory(at: archiveDir, withIntermediateDirectories: true)
            let dest = archiveDir.appendingPathComponent(whisper.url.lastPathComponent)
            if fm.fileExists(atPath: dest.path) {
                try? fm.removeItem(at: dest)
            }
            if fm.fileExists(atPath: whisper.url.path) {
                try fm.moveItem(at: whisper.url, to: dest)
            }
        }.value
    }

    /// Shell out to `cs drop "<text>"` — the universal Inbox gesture.
    ///
    /// Called by the "Drop…" menubar entry and any other pilot surface
    /// that lands an operator thought without ceremony. Auto-applies
    /// the `source:drop` origin tag via the `cs drop` verb's built-in
    /// defaults; returns the new molecule id parsed from `--json`
    /// stdout (empty when parsing fails but the verb succeeded).
    ///
    /// See task-20260424-86e9 (C-DROP-GESTURE).
    @discardableResult
    static func drop(_ text: String) async throws -> String {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            throw CosmonError.parseFailure("drop text is empty")
        }
        let result = try await run(["--json", "drop", trimmed])
        guard result.status == 0 else {
            throw CosmonError.executionFailed(
                exitCode: result.status,
                stderr: result.stderr.isEmpty ? result.stdout : result.stderr
            )
        }
        return extractMoleculeID(from: result.stdout) ?? ""
    }

    /// Shell out to `cs spark "<body>"` to transform a whisper's body into a
    /// fresh `idea` molecule. Returns the newly-created molecule id parsed
    /// out of stdout.
    @discardableResult
    static func transformWhisperToSpark(_ whisper: Whisper) async throws -> String {
        let text = whisper.body
            .trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else {
            throw CosmonError.parseFailure("whisper body is empty")
        }
        let result = try await run(["spark", text])
        guard result.status == 0 else {
            throw CosmonError.executionFailed(
                exitCode: result.status,
                stderr: result.stderr.isEmpty ? result.stdout : result.stderr
            )
        }
        return extractMoleculeID(from: result.stdout) ?? ""
    }

    // MARK: - Inbox

    /// List molecules currently pending / queued / running, by shelling out to
    /// `cs observe --json`. When `tag` is provided (e.g. `temp:hot`) it is
    /// forwarded as `--tag <glob>`.
    static func listInbox(tag: String? = nil) async throws -> [MoleculeSummary] {
        var args = ["observe", "--json"]
        if let tag, !tag.isEmpty {
            args.append(contentsOf: ["--tag", tag])
        }
        let result = try await run(args)
        guard result.status == 0 else {
            throw CosmonError.executionFailed(
                exitCode: result.status,
                stderr: result.stderr.isEmpty ? result.stdout : result.stderr
            )
        }
        guard let data = result.stdout.data(using: .utf8) else { return [] }
        guard let raw = try? JSONSerialization.jsonObject(with: data) as? [[String: Any]] else {
            return []
        }
        let open: Set<String> = ["pending", "queued", "running", "active"]
        var out: [MoleculeSummary] = []
        for obj in raw {
            guard let id = obj["id"] as? String,
                  let formula = obj["formula"] as? String,
                  let status = obj["status"] as? String,
                  open.contains(status) else {
                continue
            }
            let worker = obj["worker"] as? String
            out.append(MoleculeSummary(
                id: id,
                formula: formula,
                status: status,
                worker: worker,
                tags: []
            ))
        }
        return out
    }

    /// Fetch full details for a single molecule (topic + tags + moleculeDir).
    static func moleculeDetail(id: String) async throws -> MoleculeDetail {
        let result = try await run(["observe", id, "--json"])
        guard result.status == 0 else {
            throw CosmonError.executionFailed(
                exitCode: result.status,
                stderr: result.stderr.isEmpty ? result.stdout : result.stderr
            )
        }
        guard let data = result.stdout.data(using: .utf8),
              let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let mid = obj["id"] as? String else {
            throw CosmonError.parseFailure("could not parse `cs observe <id> --json` output")
        }
        let variables = obj["variables"] as? [String: Any] ?? [:]
        let topic = (variables["topic"] as? String) ?? ""
        let tags = (obj["tags"] as? [String]) ?? []
        let formula = (obj["formula"] as? String) ?? ""
        let status = (obj["status"] as? String) ?? "unknown"
        let worker = obj["worker"] as? String
        let moleculeDir = obj["molecule_dir"] as? String
        let typedLinks = parseTypedLinks(obj["typed_links"] as? [[String: Any]] ?? [])
        return MoleculeDetail(
            id: mid,
            formula: formula,
            status: status,
            topic: topic,
            tags: tags,
            moleculeDir: moleculeDir,
            worker: worker,
            createdAt: parseOptionalDate(obj["created_at"] as? String),
            updatedAt: parseOptionalDate(obj["updated_at"] as? String),
            typedLinks: typedLinks
        )
    }

    /// Molecule-id shape used by `cs` — e.g. `task-20260423-cefe`.
    /// Anything not matching is treated as free-form (URL, previous
    /// kind) and rendered without a click target.
    private static let moleculeIDRegex: NSRegularExpression? = {
        try? NSRegularExpression(
            pattern: #"^(task|idea|issue|decision|signal|deliberation|delib|spark|const|constellation|adr)-\d{8}-[0-9a-f]{4}$"#
        )
    }()

    private static func looksLikeMoleculeID(_ s: String) -> Bool {
        guard let re = moleculeIDRegex else { return false }
        let ns = s as NSString
        return re.firstMatch(in: s, range: NSRange(location: 0, length: ns.length)) != nil
    }

    /// Parse the raw `typed_links` JSON array into Swift values.
    ///
    /// The wire shape matches Rust's `MoleculeLink`:
    /// `{ "rel": "blocks", "target": "task-..." }`,
    /// `{ "rel": "blocked_by", "source": "task-..." }`,
    /// `{ "rel": "decayed_from", "id": "const-..." }`,
    /// `{ "rel": "merged_from", "ids": ["a", "b"] }`,
    /// `{ "rel": "transformed_from", "kind": "task" }`,
    /// `{ "rel": "entangled", "target": "https://…" }`.
    ///
    /// Unknown `rel` values are dropped silently — the detail pane
    /// should never crash on a forward-compat payload.
    private static func parseTypedLinks(_ raw: [[String: Any]]) -> [MoleculeTypedLink] {
        var out: [MoleculeTypedLink] = []
        for obj in raw {
            guard let rel = obj["rel"] as? String,
                  let relation = MoleculeTypedLinkRelation(rawValue: rel) else {
                continue
            }
            switch relation {
            case .blocks, .refines:
                if let target = obj["target"] as? String {
                    out.append(MoleculeTypedLink(
                        relation: relation,
                        target: target,
                        targetIsMolecule: looksLikeMoleculeID(target)
                    ))
                }
            case .blockedBy, .refinedBy:
                if let source = obj["source"] as? String {
                    out.append(MoleculeTypedLink(
                        relation: relation,
                        target: source,
                        targetIsMolecule: looksLikeMoleculeID(source)
                    ))
                }
            case .decayedFrom, .decayProduct, .mergedInto:
                if let id = obj["id"] as? String {
                    out.append(MoleculeTypedLink(
                        relation: relation,
                        target: id,
                        targetIsMolecule: looksLikeMoleculeID(id)
                    ))
                }
            case .mergedFrom:
                if let ids = obj["ids"] as? [String] {
                    for id in ids {
                        out.append(MoleculeTypedLink(
                            relation: relation,
                            target: id,
                            targetIsMolecule: looksLikeMoleculeID(id)
                        ))
                    }
                }
            case .transformedFrom:
                if let kind = obj["kind"] as? String {
                    // `kind` is a MoleculeKind string (e.g. "task"), not
                    // a molecule id — render as plain text.
                    out.append(MoleculeTypedLink(
                        relation: relation,
                        target: kind,
                        targetIsMolecule: false
                    ))
                }
            case .entangled:
                if let target = obj["target"] as? String {
                    out.append(MoleculeTypedLink(
                        relation: relation,
                        target: target,
                        targetIsMolecule: looksLikeMoleculeID(target)
                    ))
                }
            }
        }
        return out
    }

    /// Shell out to `cs tackle <id> --leaf`. Non-blocking: the tackle
    /// command itself returns once the worker is spawned into a tmux session.
    static func tackle(moleculeID: String) async throws {
        let result = try await run(["tackle", moleculeID, "--leaf"])
        guard result.status == 0 else {
            throw CosmonError.executionFailed(
                exitCode: result.status,
                stderr: result.stderr.isEmpty ? result.stdout : result.stderr
            )
        }
    }

    /// Shell out to `cs whisper <id> -m <body>` to inject a perturbation
    /// payload into a live worker's tmux pane.
    ///
    /// Rejects empty / whitespace-only bodies locally so we do not spend
    /// a process spawn on a no-op. Any non-zero exit from the CLI
    /// (rate-limited, size-limited, session-mismatch, missing molecule)
    /// is surfaced verbatim as `CosmonError.executionFailed` with the
    /// stderr text so the UI can render a useful message.
    static func whisper(moleculeID: String, body: String) async throws {
        let trimmed = body.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            throw CosmonError.parseFailure("empty whisper body")
        }
        let result = try await run(["whisper", moleculeID, "-m", trimmed])
        guard result.status == 0 else {
            throw CosmonError.executionFailed(
                exitCode: result.status,
                stderr: result.stderr.isEmpty ? result.stdout : result.stderr
            )
        }
    }

    /// Shell out to `cs collapse <id> --reason <reason>`.
    static func collapse(moleculeID: String, reason: String) async throws {
        let r = reason.trimmingCharacters(in: .whitespacesAndNewlines)
        let args = ["collapse", moleculeID, "--reason", r.isEmpty ? "collapsed from mac-pilot" : r]
        let result = try await run(args)
        guard result.status == 0 else {
            throw CosmonError.executionFailed(
                exitCode: result.status,
                stderr: result.stderr.isEmpty ? result.stdout : result.stderr
            )
        }
    }

    // MARK: - Per-galaxy (Skylight)

    /// List open molecules inside the galaxy rooted at `galaxyPath`. Used by
    /// the Skylight molecule picker. Scopes the `cs observe --json` invocation
    /// to the galaxy's own `.cosmon/` by setting the process cwd.
    static func listInboxIn(galaxyPath: URL, tag: String? = nil) async throws -> [MoleculeSummary] {
        var args = ["observe", "--json"]
        if let tag, !tag.isEmpty {
            args.append(contentsOf: ["--tag", tag])
        }
        let result = try await runAt(cwd: galaxyPath, args: args)
        guard result.status == 0 else {
            throw CosmonError.executionFailed(
                exitCode: result.status,
                stderr: result.stderr.isEmpty ? result.stdout : result.stderr
            )
        }
        guard let data = result.stdout.data(using: .utf8),
              let raw = try? JSONSerialization.jsonObject(with: data) as? [[String: Any]] else {
            return []
        }
        let open: Set<String> = ["pending", "queued", "running", "active"]
        var out: [MoleculeSummary] = []
        for obj in raw {
            guard let id = obj["id"] as? String,
                  let formula = obj["formula"] as? String,
                  let status = obj["status"] as? String,
                  open.contains(status) else {
                continue
            }
            let worker = obj["worker"] as? String
            out.append(MoleculeSummary(
                id: id,
                formula: formula,
                status: status,
                worker: worker,
                tags: []
            ))
        }
        return out
    }

    /// Emit a whisper into a live worker of `galaxyPath`. Thin wrapper around
    /// `cs whisper <mol> --message <body>`. The CLI fails closed if the
    /// target pane's foreground command is not whitelisted (ADR-038) —
    /// surface that as a `CosmonError.executionFailed` verbatim.
    static func whisper(galaxyPath: URL, moleculeID: String, body: String) async throws {
        let trimmed = body.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            throw CosmonError.parseFailure("whisper body is empty")
        }
        let args = ["whisper", moleculeID, "--message", trimmed]
        let result = try await runAt(cwd: galaxyPath, args: args)
        guard result.status == 0 else {
            throw CosmonError.executionFailed(
                exitCode: result.status,
                stderr: result.stderr.isEmpty ? result.stdout : result.stderr
            )
        }
    }

    // MARK: - Galaxies

    /// Scan `/srv/cosmon/*/` for any directory containing a `.cosmon/` folder.
    static func listGalaxies(galaxiesRoot: URL = galaxiesRoot) async throws -> [Galaxy] {
        try await Task.detached(priority: .userInitiated) { () -> [Galaxy] in
            let fm = FileManager.default
            guard fm.fileExists(atPath: galaxiesRoot.path) else { return [] }
            let children = (try? fm.contentsOfDirectory(
                at: galaxiesRoot,
                includingPropertiesForKeys: [.isDirectoryKey, .contentModificationDateKey],
                options: [.skipsHiddenFiles]
            )) ?? []
            var out: [Galaxy] = []
            for url in children {
                var isDir: ObjCBool = false
                guard fm.fileExists(atPath: url.path, isDirectory: &isDir), isDir.boolValue else {
                    continue
                }
                let cosmonDir = url.appendingPathComponent(".cosmon", isDirectory: true)
                guard fm.fileExists(atPath: cosmonDir.path) else { continue }
                let pending = countPendingMolecules(at: cosmonDir)
                let activity = latestMtime(at: cosmonDir)
                out.append(Galaxy(
                    name: url.lastPathComponent,
                    path: url,
                    pendingCount: pending,
                    lastActivity: activity
                ))
            }
            return out.sorted { $0.name.lowercased() < $1.name.lowercased() }
        }.value
    }

    // MARK: - Motion

    /// Shell out to `cs motion --json` and decode the live cluster snapshot.
    /// Non-zero exit codes and parse failures surface as `CosmonError`.
    static func motion(window: String = "15m") async throws -> MotionSnapshot {
        let result = try await run(["motion", "--json", "--window", window])
        guard result.status == 0 else {
            throw CosmonError.executionFailed(
                exitCode: result.status,
                stderr: result.stderr.isEmpty ? result.stdout : result.stderr
            )
        }
        guard let data = result.stdout.data(using: .utf8),
              let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            throw CosmonError.parseFailure("could not parse `cs motion --json` output")
        }
        return MotionDecoder.decode(obj)
    }

    /// Best-effort open the given galaxy in a terminal. Tries Ghostty first,
    /// falls back to Terminal.app. Swallows failures (best-effort UX).
    static func openInTerminal(galaxyPath: URL) {
        let task = Process()
        task.executableURL = URL(fileURLWithPath: "/usr/bin/env")
        task.arguments = ["open", "-a", "Ghostty", "-n", galaxyPath.path]
        do {
            try task.run()
        } catch {
            let fallback = Process()
            fallback.executableURL = URL(fileURLWithPath: "/usr/bin/env")
            fallback.arguments = ["open", "-a", "Terminal", galaxyPath.path]
            try? fallback.run()
        }
    }

    /// Open the given path in Finder.
    static func openInFinder(path: URL) {
        let task = Process()
        task.executableURL = URL(fileURLWithPath: "/usr/bin/env")
        task.arguments = ["open", path.path]
        try? task.run()
    }

    // MARK: - Helpers (new)

    private static func parseOptionalDate(_ s: String?) -> Date? {
        guard let s else { return nil }
        let iso = ISO8601DateFormatter()
        iso.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        if let d = iso.date(from: s) { return d }
        iso.formatOptions = [.withInternetDateTime]
        return iso.date(from: s)
    }

    private static func extractMoleculeID(from stdout: String) -> String? {
        let pattern = #"(?:task|idea|issue|decision|signal|deliberation|delib|spark|const|constellation|adr)-\d{8}-[0-9a-f]{4}"#
        guard let regex = try? NSRegularExpression(pattern: pattern) else { return nil }
        let ns = stdout as NSString
        let m = regex.firstMatch(in: stdout, range: NSRange(location: 0, length: ns.length))
        guard let m else { return nil }
        return ns.substring(with: m.range)
    }

    private static func countPendingMolecules(at cosmonDir: URL) -> Int {
        let fm = FileManager.default
        let moleculesDir = cosmonDir
            .appendingPathComponent("state/fleets/default/molecules", isDirectory: true)
        guard fm.fileExists(atPath: moleculesDir.path) else { return 0 }
        let children = (try? fm.contentsOfDirectory(at: moleculesDir, includingPropertiesForKeys: nil)) ?? []
        var count = 0
        for dir in children {
            let stateFile = dir.appendingPathComponent("state.json")
            guard let data = try? Data(contentsOf: stateFile),
                  let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else {
                continue
            }
            if let status = obj["status"] as? String,
               ["pending", "queued", "running", "active"].contains(status) {
                count += 1
            }
        }
        return count
    }

    private static func latestMtime(at cosmonDir: URL) -> Date? {
        let fm = FileManager.default
        let moleculesDir = cosmonDir
            .appendingPathComponent("state/fleets/default/molecules", isDirectory: true)
        guard fm.fileExists(atPath: moleculesDir.path) else {
            return (try? fm.attributesOfItem(atPath: cosmonDir.path)[.modificationDate]) as? Date
        }
        let children = (try? fm.contentsOfDirectory(at: moleculesDir, includingPropertiesForKeys: [.contentModificationDateKey])) ?? []
        var latest: Date?
        for dir in children {
            if let values = try? dir.resourceValues(forKeys: [.contentModificationDateKey]),
               let mtime = values.contentModificationDate {
                if latest == nil || mtime > latest! { latest = mtime }
            }
        }
        return latest
    }
}

// MARK: - Motion decoder

/// Decoder for the JSON object returned by `cs motion --json` and the
/// `/motion` HTTP endpoint. Missing fields fall back to safe defaults so
/// the view always gets a well-formed `MotionSnapshot`.
enum MotionDecoder {
    static func decode(_ obj: [String: Any]) -> MotionSnapshot {
        let timestamp = (obj["timestamp"] as? String) ?? ""
        let window = (obj["window"] as? String) ?? ""
        let scanned = (obj["galaxies_scanned"] as? [String]) ?? []
        let workers = (obj["workers"] as? [[String: Any]] ?? [])
            .map(decodeWorker)
        let molecules = (obj["running_molecules"] as? [[String: Any]] ?? [])
            .map(decodeMolecule)
        let commits = (obj["recent_git_commits"] as? [[String: Any]] ?? [])
            .map(decodeCommit)
        let whispers = (obj["recent_whispers"] as? [[String: Any]] ?? [])
            .map(decodeWhisper)
        let sparks = (obj["recent_sparks"] as? [[String: Any]] ?? [])
            .map(decodeSpark)
        return MotionSnapshot(
            timestamp: timestamp,
            window: window,
            galaxiesScanned: scanned,
            workers: workers,
            runningMolecules: molecules,
            recentCommits: commits,
            recentWhispers: whispers,
            recentSparks: sparks
        )
    }

    private static func decodeWorker(_ o: [String: Any]) -> MotionWorker {
        MotionWorker(
            name: o["name"] as? String ?? "?",
            galaxy: o["galaxy"] as? String ?? "-",
            moleculeID: o["molecule_id"] as? String,
            role: o["role"] as? String,
            status: o["status"] as? String,
            lastHeartbeat: o["last_heartbeat"] as? String,
            costUSD: o["cost_usd"] as? Double,
            repo: o["repo"] as? String
        )
    }

    private static func decodeMolecule(_ o: [String: Any]) -> MotionMolecule {
        let tags = (o["tags"] as? [String]) ?? []
        return MotionMolecule(
            id: o["id"] as? String ?? "?",
            galaxy: o["galaxy"] as? String ?? "-",
            kind: o["kind"] as? String ?? "task",
            currentStep: o["current_step"] as? Int,
            totalSteps: o["total_steps"] as? Int,
            lastEvolveAt: o["last_evolve_at"] as? String,
            tags: tags,
            topicPreview: o["topic_preview"] as? String,
            assignedWorker: o["assigned_worker"] as? String
        )
    }

    private static func decodeCommit(_ o: [String: Any]) -> MotionCommit {
        MotionCommit(
            galaxy: o["galaxy"] as? String ?? "-",
            sha: o["sha"] as? String ?? "?",
            subject: o["subject"] as? String ?? "",
            timestamp: o["timestamp"] as? String ?? "",
            author: o["author"] as? String ?? ""
        )
    }

    private static func decodeWhisper(_ o: [String: Any]) -> MotionWhisper {
        MotionWhisper(
            id: o["id"] as? String ?? "?",
            galaxy: o["galaxy"] as? String ?? "-",
            senderNucleonID: o["sender_nucleon_id"] as? String,
            receivedAt: o["received_at"] as? String ?? "",
            bodyPreview: o["body_preview"] as? String ?? ""
        )
    }

    private static func decodeSpark(_ o: [String: Any]) -> MotionSpark {
        let tags = (o["tags"] as? [String]) ?? []
        return MotionSpark(
            id: o["id"] as? String ?? "?",
            galaxy: o["galaxy"] as? String ?? "-",
            createdAt: o["created_at"] as? String ?? "",
            topicPreview: o["topic_preview"] as? String,
            tags: tags
        )
    }
}

// MARK: - Whisper parser

/// Parser for whisper `.md` files written by the Matrix ingress.
///
/// Shape:
/// ```
/// ---
/// sender_mxid: "@…"
/// origin_server_ts: 1776891587880
/// received_at: "2026-04-22T21:32:37Z"
/// …
/// ---
///
/// <body text>
/// ```
enum WhisperParser {

    static func parse(body raw: String, url: URL, roomDirectoryName: String) -> Whisper? {
        let lines = raw.components(separatedBy: "\n")
        guard lines.first == "---" else { return nil }
        var i = 1
        var fm: [String: String] = [:]
        while i < lines.count && lines[i] != "---" {
            if let (k, v) = parseKeyValue(lines[i]) {
                fm[k] = v
            }
            i += 1
        }
        guard i < lines.count else { return nil }
        let bodyStart = i + 1
        let bodyText = Array(lines[bodyStart...])
            .joined(separator: "\n")
            .trimmingCharacters(in: .whitespacesAndNewlines)

        let receivedAt: Date
        if let iso = fm["received_at"], let d = parseISO(iso) {
            receivedAt = d
        } else if let ts = fm["origin_server_ts"].flatMap(Int64.init) {
            receivedAt = Date(timeIntervalSince1970: TimeInterval(ts) / 1000.0)
        } else if let mtime = (try? url.resourceValues(forKeys: [.contentModificationDateKey]))?
            .contentModificationDate {
            receivedAt = mtime
        } else {
            receivedAt = Date()
        }

        return Whisper(
            url: url,
            roomDirectoryName: roomDirectoryName,
            frontmatter: fm,
            body: bodyText,
            receivedAt: receivedAt
        )
    }

    private static func parseKeyValue(_ line: String) -> (String, String)? {
        guard let colon = line.firstIndex(of: ":") else { return nil }
        let key = String(line[..<colon]).trimmingCharacters(in: .whitespaces)
        var value = String(line[line.index(after: colon)...])
            .trimmingCharacters(in: .whitespaces)
        if value.count >= 2, value.hasPrefix("\""), value.hasSuffix("\"") {
            value = String(value.dropFirst().dropLast())
        }
        return (key, value)
    }

    private static func parseISO(_ s: String) -> Date? {
        let iso = ISO8601DateFormatter()
        iso.formatOptions = [.withInternetDateTime]
        if let d = iso.date(from: s) { return d }
        iso.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return iso.date(from: s)
    }
}

// MARK: - Session parser

/// Parser for the on-disk session markdown layout. Extracted for unit-testability
/// even though we ship no automated tests v0 — future XCTest bundles can exercise
/// `SessionParser.parseOpen` directly without a live `cs` binary.
enum SessionParser {

    /// Returns a `SessionState` if the body represents an *open* session
    /// (no closing frontmatter with `ended_at:`), otherwise `nil`.
    static func parseOpen(body: String) -> SessionState? {
        let lines = body.components(separatedBy: "\n")
        guard let header = extractHeader(lines: lines) else { return nil }

        let footer = extractFooter(lines: lines, afterHeaderEnd: header.endIndex)
        if footer.contains(where: { $0.hasPrefix("ended_at:") }) {
            return nil
        }

        var sessionID: SessionID?
        var startedAt: Date?
        for line in header.lines {
            if let v = yamlValue(line, key: "session_id") {
                sessionID = SessionID(raw: v)
            } else if let v = yamlValue(line, key: "started_at") {
                startedAt = parseISODate(v)
            }
        }
        guard let sid = sessionID, let ts = startedAt else { return nil }

        let bodyLines = Array(lines[header.endIndex..<footerStart(lines: lines, afterHeaderEnd: header.endIndex)])
        let notes = parseNotes(from: bodyLines)
        return SessionState(sessionID: sid, startedAt: ts, notes: notes)
    }

    // MARK: - Internals

    private struct Header {
        let lines: [String]
        /// Line index *after* the closing `---` of the opening frontmatter block.
        let endIndex: Int
    }

    private static func extractHeader(lines: [String]) -> Header? {
        guard lines.first == "---" else { return nil }
        var headerLines: [String] = []
        var i = 1
        while i < lines.count && lines[i] != "---" {
            headerLines.append(lines[i])
            i += 1
        }
        guard i < lines.count else { return nil }
        return Header(lines: headerLines, endIndex: i + 1)
    }

    /// Returns the line index where a potential closing frontmatter block starts.
    /// This is the index of the last `---` line in `lines[afterHeaderEnd...]`
    /// that is followed by a key-value block ending with another `---`.
    /// If no closing block is detected, returns `lines.count`.
    private static func footerStart(lines: [String], afterHeaderEnd: Int) -> Int {
        // Find the last pair of `---` markers in the suffix.
        var last = -1
        var i = afterHeaderEnd
        while i < lines.count {
            if lines[i] == "---" {
                // Look for a matching closing `---` before EOF.
                var j = i + 1
                while j < lines.count && lines[j] != "---" { j += 1 }
                if j < lines.count {
                    last = i
                    i = j + 1
                    continue
                }
            }
            i += 1
        }
        return last == -1 ? lines.count : last
    }

    private static func extractFooter(lines: [String], afterHeaderEnd: Int) -> [String] {
        let start = footerStart(lines: lines, afterHeaderEnd: afterHeaderEnd)
        guard start < lines.count else { return [] }
        var out: [String] = []
        var i = start + 1
        while i < lines.count && lines[i] != "---" {
            out.append(lines[i])
            i += 1
        }
        return out
    }

    private static func parseNotes(from lines: [String]) -> [Note] {
        var notes: [Note] = []
        var currentTs: String?
        var currentTag: String?
        var buffer: [String] = []

        // Matches `## HH:MM:SS — tag` where the em-dash may be `—` or `-`
        // and the tag may be missing.
        let headerRegex = try? NSRegularExpression(
            pattern: #"^##\s+(\d{2}:\d{2}:\d{2})(?:\s+[—-]\s*(\S.*)?)?$"#
        )

        func flush() {
            guard let ts = currentTs else { return }
            let text = buffer.joined(separator: "\n")
                .trimmingCharacters(in: .whitespacesAndNewlines)
            notes.append(Note(timestamp: ts, tag: currentTag, text: text))
            buffer.removeAll()
        }

        for line in lines {
            if let regex = headerRegex {
                let ns = line as NSString
                if let m = regex.firstMatch(in: line, range: NSRange(location: 0, length: ns.length)) {
                    flush()
                    currentTs = ns.substring(with: m.range(at: 1))
                    let tagRange = m.range(at: 2)
                    if tagRange.location != NSNotFound {
                        let t = ns.substring(with: tagRange).trimmingCharacters(in: .whitespaces)
                        currentTag = t.isEmpty ? nil : t
                    } else {
                        currentTag = nil
                    }
                    continue
                }
            }
            if currentTs != nil {
                buffer.append(line)
            }
        }
        flush()
        return notes
    }

    private static func yamlValue(_ line: String, key: String) -> String? {
        let prefix = "\(key):"
        guard line.hasPrefix(prefix) else { return nil }
        var v = String(line.dropFirst(prefix.count))
        v = v.trimmingCharacters(in: .whitespaces)
        if v.count >= 2, v.hasPrefix("\""), v.hasSuffix("\"") {
            v = String(v.dropFirst().dropLast())
        }
        return v
    }

    private static func parseISODate(_ s: String) -> Date? {
        let iso = ISO8601DateFormatter()
        iso.formatOptions = [.withInternetDateTime]
        if let d = iso.date(from: s) { return d }
        iso.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return iso.date(from: s)
    }
}
