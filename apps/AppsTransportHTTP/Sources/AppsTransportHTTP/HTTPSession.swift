// SPDX-License-Identifier: MPL-2.0
//
// HTTPSession — narrow protocol over URLSession so tests can substitute
// an in-memory mock without standing up a real socket.
//
// The shape mirrors `URLSession.data(for:)` so the production
// implementation is one line.

import Foundation

public protocol HTTPSession: Sendable {
    func data(for request: URLRequest) async throws -> (Data, URLResponse)
}

extension URLSession: HTTPSession {
    // URLSession already conforms in shape; this is the explicit
    // adapter so the Sendable bound holds. URLSession is documented
    // as thread-safe and Sendable-compatible.
    @available(macOS 12.0, iOS 15.0, *)
    public func data(for request: URLRequest) async throws -> (Data, URLResponse) {
        return try await self.data(for: request, delegate: nil)
    }
}
