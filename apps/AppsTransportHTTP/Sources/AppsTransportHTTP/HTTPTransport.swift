// SPDX-License-Identifier: MPL-2.0
//
// HTTPTransport — narrow async client over an `HTTPSession`.
//
// Generic `get<T>` / `post<Req, Resp>` shapes; no app-specific schema
// leaks into the package. The client routes errors into typed
// `HTTPTransportError` cases so SwiftUI views can switch on the
// failure kind rather than parse strings. Reconnect/backoff is
// exposed as `withReconnect` so calling sites stay explicit about
// retry intent.

import Foundation

public actor HTTPTransport {
    public let config: HTTPTransportConfig
    private let session: any HTTPSession
    private let encoder: JSONEncoder
    private let decoder: JSONDecoder

    public init(
        config: HTTPTransportConfig = HTTPTransportConfig(),
        session: (any HTTPSession)? = nil,
        encoder: JSONEncoder = HTTPTransportConfig.makeEncoder(),
        decoder: JSONDecoder = HTTPTransportConfig.makeDecoder()
    ) {
        self.config = config
        if let s = session {
            self.session = s
        } else {
            let cfg = URLSessionConfiguration.ephemeral
            cfg.timeoutIntervalForRequest = config.timeout
            cfg.timeoutIntervalForResource = config.timeout * 4
            self.session = URLSession(configuration: cfg)
        }
        self.encoder = encoder
        self.decoder = decoder
    }

    // MARK: - Verbs

    public func get<T: Decodable & Sendable>(_ path: String) async throws -> T {
        let url = self.url(for: path)
        var req = URLRequest(url: url)
        req.httpMethod = "GET"
        req.timeoutInterval = config.timeout
        return try await execute(req)
    }

    public func post<Req: Encodable & Sendable, Resp: Decodable & Sendable>(
        _ path: String,
        body: Req
    ) async throws -> Resp {
        let url = self.url(for: path)
        var req = URLRequest(url: url)
        req.httpMethod = "POST"
        req.timeoutInterval = config.timeout
        req.setValue("application/json; charset=utf-8", forHTTPHeaderField: "Content-Type")
        do {
            req.httpBody = try encoder.encode(body)
        } catch {
            throw HTTPTransportError.protocolMismatch(reason: "encode request body: \(error)")
        }
        return try await execute(req)
    }

    public func delete(_ path: String) async throws {
        let url = self.url(for: path)
        var req = URLRequest(url: url)
        req.httpMethod = "DELETE"
        req.timeoutInterval = config.timeout
        let _: EmptyResponse = try await execute(req)
    }

    // MARK: - Reconnect

    /// Run `body` with exponential backoff on `daemonOffline` errors,
    /// up to `maxAttempts`. All other error kinds propagate immediately
    /// — they are not transient.
    public func withReconnect<T: Sendable>(
        maxAttempts: Int = 5,
        operation: @Sendable () async throws -> T
    ) async throws -> T {
        var attempt = 0
        var delay = config.initialBackoff
        while true {
            do {
                return try await operation()
            } catch let err as HTTPTransportError {
                attempt += 1
                if case .daemonOffline = err, attempt < maxAttempts {
                    try await Task.sleep(nanoseconds: UInt64(delay * 1_000_000_000))
                    delay = min(delay * 2.0, config.maxBackoff)
                    continue
                }
                throw err
            }
        }
    }

    // MARK: - Plumbing

    private func url(for path: String) -> URL {
        // `URL.appendingPathComponent` percent-encodes the slash, which
        // breaks `/v1/foo/bar` style routes. Use the explicit
        // `normalizedAppend` helper instead — it appends to the URL's
        // `absoluteString` and re-parses, preserving the path
        // separators verbatim.
        config.baseURL.normalizedAppend(path: path)
    }

    private func execute<T: Decodable & Sendable>(_ req: URLRequest) async throws -> T {
        let data: Data
        let response: URLResponse
        do {
            (data, response) = try await session.data(for: req)
        } catch let error as URLError {
            switch error.code {
            case .cancelled:
                throw HTTPTransportError.cancelled
            case .cannotConnectToHost,
                 .cannotFindHost,
                 .networkConnectionLost,
                 .notConnectedToInternet,
                 .timedOut,
                 .dnsLookupFailed:
                throw HTTPTransportError.daemonOffline(reason: error.localizedDescription)
            default:
                throw HTTPTransportError.daemonOffline(reason: error.localizedDescription)
            }
        } catch {
            throw HTTPTransportError.daemonOffline(reason: "\(error)")
        }
        guard let http = response as? HTTPURLResponse else {
            throw HTTPTransportError.nonHTTPResponse
        }
        let status = http.statusCode
        if (200..<300).contains(status) {
            if T.self == EmptyResponse.self {
                // SAFETY: T is statically EmptyResponse here.
                return EmptyResponse() as! T
            }
            do {
                return try decoder.decode(T.self, from: data)
            } catch {
                throw HTTPTransportError.protocolMismatch(reason: "decode response: \(error)")
            }
        }
        // Non-2xx: try canonical error body, else surface raw.
        if let body = try? decoder.decode(ApplicationErrorBody.self, from: data) {
            throw HTTPTransportError.applicationError(status: status, body: body)
        }
        let raw = String(data: data, encoding: .utf8) ?? ""
        throw HTTPTransportError.unexpectedStatus(status: status, raw: raw)
    }
}

/// Sentinel type for endpoints that return no body (DELETE, etc.).
public struct EmptyResponse: Codable, Sendable {
    public init() {}
}

// MARK: - URL helpers

private extension URL {
    /// `URL.appendingPathComponent` percent-encodes the slash, which
    /// breaks `/v1/foo/bar` style routes. Re-implement by appending
    /// to `.absoluteString` and re-parsing — a small but correct hack.
    func normalizedAppend(path: String) -> URL {
        var s = self.absoluteString
        if s.hasSuffix("/") { s.removeLast() }
        let trimmed = path.hasPrefix("/") ? path : "/" + path
        let combined = s + trimmed
        return URL(string: combined) ?? self
    }
}
