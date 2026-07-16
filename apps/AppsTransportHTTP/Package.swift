// swift-tools-version: 5.9
// SPDX-License-Identifier: MPL-2.0

import PackageDescription

// AppsTransportHTTP — Swift counterpart to the Rust `apps-transport-http`
// crate. Every native app in the local cluster of galaxies (Verdict,
// Mur du Matin, Cosmon, future) talks to its cluster-side daemon
// through `HTTPTransport` so error-routing, JSON coding conventions
// and reconnect/backoff stay uniform.
//
// Wire convention: HTTP-on-Tailscale, JSON, snake_case keys, dates as
// `seconds_since_1970` doubles. URL prefix `/v1/<resource>` — bumping
// to `/v2/` is the protocol's semver lever.

let package = Package(
    name: "AppsTransportHTTP",
    platforms: [
        .macOS(.v13),
        .iOS(.v16),
    ],
    products: [
        .library(
            name: "AppsTransportHTTP",
            targets: ["AppsTransportHTTP"]
        ),
    ],
    targets: [
        .target(
            name: "AppsTransportHTTP",
            path: "Sources/AppsTransportHTTP"
        ),
        .testTarget(
            name: "AppsTransportHTTPTests",
            dependencies: ["AppsTransportHTTP"],
            path: "Tests/AppsTransportHTTPTests"
        ),
    ]
)
