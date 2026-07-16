// SPDX-License-Identifier: MPL-2.0
//
// HTTPTransportConfig — host, port, timeout, backoff and JSON coding
// strategy. Defaults target the canonical local-cluster daemon
// (host.example:8789) so every cluster app converges on
// the same wire shape. Override via `Info.plist` keys
// `CockpitHost` / `CockpitPort`, or programmatically.

import Foundation

public struct HTTPTransportConfig: Sendable {
    public var host: String
    public var port: Int
    /// Request timeout, in seconds.
    public var timeout: TimeInterval
    /// Initial reconnect/backoff delay, in seconds. Subsequent delays
    /// grow exponentially up to `maxBackoff`.
    public var initialBackoff: TimeInterval
    public var maxBackoff: TimeInterval
    /// Maximum poll rate (Hz). The transport sleeps between polls to
    /// keep the rate ≤ this. `nil` disables rate-limiting.
    public var maxPollHz: Double?

    public static let defaultHost = "host.example"
    public static let defaultPort = 8789

    public init(
        host: String = HTTPTransportConfig.defaultHost,
        port: Int = HTTPTransportConfig.defaultPort,
        timeout: TimeInterval = 5.0,
        initialBackoff: TimeInterval = 0.5,
        maxBackoff: TimeInterval = 30.0,
        maxPollHz: Double? = nil
    ) {
        self.host = host
        self.port = port
        self.timeout = timeout
        self.initialBackoff = initialBackoff
        self.maxBackoff = maxBackoff
        self.maxPollHz = maxPollHz
    }

    /// Resolve a config from `Info.plist` keys `CockpitHost` /
    /// `CockpitPort`, falling back to the cluster defaults.
    public static func fromInfoPlist(_ bundle: Bundle = .main) -> HTTPTransportConfig {
        let host = (bundle.object(forInfoDictionaryKey: "CockpitHost") as? String) ?? defaultHost
        let port: Int
        if let v = bundle.object(forInfoDictionaryKey: "CockpitPort") {
            if let n = v as? Int { port = n }
            else if let s = v as? String, let n = Int(s) { port = n }
            else { port = defaultPort }
        } else {
            port = defaultPort
        }
        return HTTPTransportConfig(host: host, port: port)
    }

    /// Build the canonical base URL for `/v1/<resource>` paths.
    public var baseURL: URL {
        // `URL(string:)` rejects bare hostnames with ports if we're not
        // careful; build the components manually.
        var components = URLComponents()
        components.scheme = "http"
        components.host = host
        components.port = port
        components.path = ""
        // Force-unwrap is safe: we built every field.
        return components.url!
    }

    /// Encoder convention: snake_case keys, dates as seconds-since-epoch.
    public static func makeEncoder() -> JSONEncoder {
        let e = JSONEncoder()
        e.keyEncodingStrategy = .convertToSnakeCase
        e.dateEncodingStrategy = .secondsSince1970
        return e
    }

    /// Decoder convention: snake_case → camelCase, dates as seconds-since-epoch.
    public static func makeDecoder() -> JSONDecoder {
        let d = JSONDecoder()
        d.keyDecodingStrategy = .convertFromSnakeCase
        d.dateDecodingStrategy = .secondsSince1970
        return d
    }
}
