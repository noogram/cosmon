// SPDX-License-Identifier: MPL-2.0

import XCTest
@testable import CosmonAppKit

final class WireModelsTests: XCTestCase {
    func testMoleculeSummaryShortIDExtractsSuffix() {
        let m = MoleculeSummary(
            id: "task-20260426-aaaa", status: "running", kind: "task",
            formula: "task-work", currentStep: 0, totalSteps: 2,
            worker: nil, workerLive: nil, liveness: "unknown",
            updatedAt: Date()
        )
        XCTAssertEqual(m.shortID, "aaaa")
    }

    func testMoleculeSummaryKindGlyphForTask() {
        let m = MoleculeSummary(
            id: "task-1", status: "running", kind: "task",
            formula: "f", currentStep: 0, totalSteps: 1,
            worker: nil, workerLive: nil, liveness: "unknown",
            updatedAt: Date()
        )
        XCTAssertEqual(m.kindGlyph, "🔧")
    }
}

final class MockDaemonClientTests: XCTestCase {
    func testHealthReportsGalaxyCount() async throws {
        let client = MockDaemonClient()
        let h = try await client.health()
        XCTAssertEqual(h.galaxiesCount, 2)
        XCTAssertTrue(h.ok)
    }

    func testListGalaxiesReturnsSeed() async throws {
        let client = MockDaemonClient()
        let g = try await client.listGalaxies()
        XCTAssertEqual(g.count, 2)
        XCTAssertEqual(g.map(\.name).sorted(), ["cosmon", "mailroom"])
    }

    func testListMoleculesFilterByStatus() async throws {
        let client = MockDaemonClient()
        let running = try await client.listMolecules(galaxy: "cosmon", status: "running")
        XCTAssertEqual(running.count, 2)
        for m in running { XCTAssertEqual(m.status, "running") }
    }

    func testMoleculeDetailIncludesBriefingAndLogTail() async throws {
        let client = MockDaemonClient()
        let detail = try await client.moleculeDetail(galaxy: "cosmon", id: "task-20260426-aaaa")
        XCTAssertEqual(detail.id, "task-20260426-aaaa")
        XCTAssertNotNil(detail.briefing)
        XCTAssertNotNil(detail.logTail)
        XCTAssertNotNil(detail.tmuxAttachHint)
    }
}
