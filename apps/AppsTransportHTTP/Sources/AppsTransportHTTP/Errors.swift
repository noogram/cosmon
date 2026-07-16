// SPDX-License-Identifier: MPL-2.0
//
// HTTPTransportError — typed routing of every wire failure.
//
// Mirrors the Rust `apps-transport-http::ApplicationError` codes on the
// other side of the wire. Decoding the canonical
// `{error, code, detail}` JSON body yields `.applicationError`; lower-
// level failures fan out to `.daemonOffline`, `.protocolMismatch`, etc.
// so SwiftUI views can switch on the case rather than parse strings.

import Foundation

public struct ApplicationErrorBody: Codable, Sendable, Equatable {
    public let error: String
    public let code: String
    public let detail: String?

    public init(error: String, code: String, detail: String? = nil) {
        self.error = error
        self.code = code
        self.detail = detail
    }
}

public enum HTTPTransportError: Error, Sendable, Equatable {
    /// Connection refused / host unreachable / DNS failure. The daemon
    /// is probably not running.
    case daemonOffline(reason: String)
    /// Server replied but the JSON body could not be decoded into the
    /// expected shape — version mismatch between client and daemon.
    case protocolMismatch(reason: String)
    /// HTTP 4xx/5xx with a canonical `{error, code, detail}` body.
    case applicationError(status: Int, body: ApplicationErrorBody)
    /// HTTP 4xx/5xx without a parseable body.
    case unexpectedStatus(status: Int, raw: String)
    /// `URLSession` returned a non-HTTP response (rare, but possible).
    case nonHTTPResponse
    /// Operation was cancelled by the caller.
    case cancelled

    public static func == (lhs: HTTPTransportError, rhs: HTTPTransportError) -> Bool {
        switch (lhs, rhs) {
        case (.daemonOffline(let a), .daemonOffline(let b)):
            return a == b
        case (.protocolMismatch(let a), .protocolMismatch(let b)):
            return a == b
        case (.applicationError(let sa, let ba), .applicationError(let sb, let bb)):
            return sa == sb && ba == bb
        case (.unexpectedStatus(let sa, let ra), .unexpectedStatus(let sb, let rb)):
            return sa == sb && ra == rb
        case (.nonHTTPResponse, .nonHTTPResponse):
            return true
        case (.cancelled, .cancelled):
            return true
        default:
            return false
        }
    }
}
