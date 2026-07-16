// SPDX-License-Identifier: MPL-2.0

import XCTest
@testable import AppsTransportHTTP

/// Mock HTTP session: returns a canned response (or throws) for each
/// request. Captures the request for assertions.
actor MockHTTPSession: HTTPSession {
    enum Behaviour: Sendable {
        case ok(status: Int, body: Data)
        case throwing(URLError.Code)
    }

    var behaviour: Behaviour = .ok(status: 200, body: Data())
    var seen: [URLRequest] = []

    func setBehaviour(_ b: Behaviour) { self.behaviour = b }
    func capturedRequests() -> [URLRequest] { seen }

    nonisolated func data(for request: URLRequest) async throws -> (Data, URLResponse) {
        // Capture and inspect under the actor.
        let beh = await self._captureAndRead(request)
        switch beh {
        case .ok(let status, let body):
            let resp = HTTPURLResponse(
                url: request.url!,
                statusCode: status,
                httpVersion: "HTTP/1.1",
                headerFields: ["Content-Type": "application/json"]
            )!
            return (body, resp)
        case .throwing(let code):
            throw URLError(code)
        }
    }

    private func _captureAndRead(_ req: URLRequest) -> Behaviour {
        seen.append(req)
        return behaviour
    }
}

struct Health: Codable, Equatable {
    let ok: Bool
    let service: String
}

struct EchoIn: Codable, Equatable {
    let name: String
}

struct EchoOut: Codable, Equatable {
    let greeting: String
}

final class HTTPTransportTests: XCTestCase {
    func makeTransport(_ session: MockHTTPSession) -> HTTPTransport {
        let cfg = HTTPTransportConfig(
            host: "localhost",
            port: 8789,
            timeout: 1.0,
            initialBackoff: 0.01,
            maxBackoff: 0.05
        )
        return HTTPTransport(config: cfg, session: session)
    }

    func testGetReturnsDecodedBody() async throws {
        let session = MockHTTPSession()
        let json = #"{"ok":true,"service":"test"}"#.data(using: .utf8)!
        await session.setBehaviour(.ok(status: 200, body: json))
        let t = makeTransport(session)
        let h: Health = try await t.get("/v1/health")
        XCTAssertEqual(h, Health(ok: true, service: "test"))
        let reqs = await session.capturedRequests()
        XCTAssertEqual(reqs.count, 1)
        XCTAssertEqual(reqs[0].httpMethod, "GET")
        XCTAssertTrue(reqs[0].url!.path.hasSuffix("/v1/health"))
    }

    func testPostEncodesBodyAndDecodesResponse() async throws {
        let session = MockHTTPSession()
        let json = #"{"greeting":"hi Carol"}"#.data(using: .utf8)!
        await session.setBehaviour(.ok(status: 200, body: json))
        let t = makeTransport(session)
        let out: EchoOut = try await t.post("/v1/echo", body: EchoIn(name: "Carol"))
        XCTAssertEqual(out.greeting, "hi Carol")
        let reqs = await session.capturedRequests()
        XCTAssertEqual(reqs[0].httpMethod, "POST")
        // Body should be snake_case via the encoder convention.
        let body = String(data: reqs[0].httpBody!, encoding: .utf8)!
        XCTAssertTrue(body.contains("\"name\":\"Carol\""), "body: \(body)")
    }

    func testApplicationErrorRoutesTo400Case() async throws {
        let session = MockHTTPSession()
        let body = #"{"error":"bad request: name required","code":"bad_request","detail":"name required"}"#.data(using: .utf8)!
        await session.setBehaviour(.ok(status: 400, body: body))
        let t = makeTransport(session)
        do {
            let _: EchoOut = try await t.post("/v1/echo", body: EchoIn(name: ""))
            XCTFail("expected throw")
        } catch let err as HTTPTransportError {
            switch err {
            case .applicationError(let status, let body):
                XCTAssertEqual(status, 400)
                XCTAssertEqual(body.code, "bad_request")
                XCTAssertEqual(body.detail, "name required")
            default:
                XCTFail("wrong case: \(err)")
            }
        }
    }

    func testNetworkUnreachableRoutesToDaemonOffline() async throws {
        let session = MockHTTPSession()
        await session.setBehaviour(.throwing(.cannotConnectToHost))
        let t = makeTransport(session)
        do {
            let _: Health = try await t.get("/v1/health")
            XCTFail("expected throw")
        } catch let err as HTTPTransportError {
            if case .daemonOffline = err { /* ok */ }
            else { XCTFail("wrong case: \(err)") }
        }
    }

    func testProtocolMismatchOnUndecodableBody() async throws {
        let session = MockHTTPSession()
        let body = #"{"unexpected":"shape"}"#.data(using: .utf8)!
        await session.setBehaviour(.ok(status: 200, body: body))
        let t = makeTransport(session)
        do {
            let _: Health = try await t.get("/v1/health")
            XCTFail("expected throw")
        } catch let err as HTTPTransportError {
            if case .protocolMismatch = err { /* ok */ }
            else { XCTFail("wrong case: \(err)") }
        }
    }

    func testUnexpectedStatusOnNonCanonicalErrorBody() async throws {
        let session = MockHTTPSession()
        let body = "not-json".data(using: .utf8)!
        await session.setBehaviour(.ok(status: 503, body: body))
        let t = makeTransport(session)
        do {
            let _: Health = try await t.get("/v1/health")
            XCTFail("expected throw")
        } catch let err as HTTPTransportError {
            if case .unexpectedStatus(let s, _) = err { XCTAssertEqual(s, 503) }
            else { XCTFail("wrong case: \(err)") }
        }
    }

    func testWithReconnectRetriesOnDaemonOffline() async throws {
        // Session that fails twice, then succeeds.
        actor FlakySession: HTTPSession {
            var failures = 2
            var seen = 0
            nonisolated func data(for request: URLRequest) async throws -> (Data, URLResponse) {
                let shouldFail = await self.advance()
                if shouldFail {
                    throw URLError(.cannotConnectToHost)
                }
                let json = #"{"ok":true,"service":"test"}"#.data(using: .utf8)!
                let resp = HTTPURLResponse(
                    url: request.url!,
                    statusCode: 200,
                    httpVersion: "HTTP/1.1",
                    headerFields: ["Content-Type": "application/json"]
                )!
                return (json, resp)
            }
            private func advance() -> Bool {
                seen += 1
                if failures > 0 {
                    failures -= 1
                    return true
                }
                return false
            }
            func observed() -> Int { seen }
        }
        let session = FlakySession()
        let cfg = HTTPTransportConfig(
            host: "localhost",
            port: 8789,
            timeout: 1.0,
            initialBackoff: 0.01,
            maxBackoff: 0.02
        )
        let t = HTTPTransport(config: cfg, session: session)
        let h: Health = try await t.withReconnect(maxAttempts: 5) {
            try await t.get("/v1/health")
        }
        XCTAssertEqual(h, Health(ok: true, service: "test"))
        let observed = await session.observed()
        XCTAssertEqual(observed, 3, "two failures then one success")
    }

    func testConfigDefaultsTargetMacBookPort8789() {
        let c = HTTPTransportConfig()
        XCTAssertEqual(c.host, "host.example")
        XCTAssertEqual(c.port, 8789)
        let url = c.baseURL
        XCTAssertEqual(url.host, "host.example")
        XCTAssertEqual(url.port, 8789)
    }

    func testGetBuildsExactPathOnce() async throws {
        // Regression: the previous URL-builder routed every request
        // through `appendingPathComponent` (which percent-encodes
        // slashes) AND `normalizedAppend`, doubling the path. The
        // server then 404'd on `/v1/health/v1/health` instead of
        // serving `/v1/health`. Lock the contract.
        let session = MockHTTPSession()
        let json = #"{"ok":true,"service":"test"}"#.data(using: .utf8)!
        await session.setBehaviour(.ok(status: 200, body: json))
        let t = makeTransport(session)
        let _: Health = try await t.get("/v1/health")
        let reqs = await session.capturedRequests()
        XCTAssertEqual(reqs.count, 1)
        let url = reqs[0].url!
        // Path must be exactly `/v1/health`, no doubling, no
        // percent-encoded slash.
        XCTAssertEqual(url.path, "/v1/health")
        XCTAssertFalse(url.absoluteString.contains("%2F"))
    }

    func testGetBuildsNestedPathExactly() async throws {
        let session = MockHTTPSession()
        let json = "[]".data(using: .utf8)!
        await session.setBehaviour(.ok(status: 200, body: json))
        let t = makeTransport(session)
        let _: [String] = try await t.get("/v1/galaxies/cosmon/molecules")
        let reqs = await session.capturedRequests()
        XCTAssertEqual(reqs[0].url!.path, "/v1/galaxies/cosmon/molecules")
    }
}
