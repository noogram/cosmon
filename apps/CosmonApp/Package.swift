// swift-tools-version: 5.9
// SPDX-License-Identifier: MPL-2.0

import PackageDescription

// CosmonAppKit — testable foundation for the CosmonApp iOS target.
//
// Hosts the `Decodable` wire models, the `DaemonClient` actor (built on
// `AppsTransportHTTP`), and the `@MainActor` `ClusterStore` consumed by
// the SwiftUI screens in `App/`. Splitting these out of the app target
// makes them reachable from `swift test` and from previews without
// dragging UIKit/SwiftUI into the package.

let package = Package(
    name: "CosmonAppKit",
    platforms: [
        .iOS(.v17),
        .macOS(.v14),
    ],
    products: [
        .library(
            name: "CosmonAppKit",
            targets: ["CosmonAppKit"]
        ),
    ],
    dependencies: [
        .package(path: "../AppsTransportHTTP"),
    ],
    targets: [
        .target(
            name: "CosmonAppKit",
            dependencies: [
                .product(name: "AppsTransportHTTP", package: "AppsTransportHTTP"),
            ],
            path: "Sources/CosmonAppKit"
        ),
        .testTarget(
            name: "CosmonAppKitTests",
            dependencies: ["CosmonAppKit"],
            path: "Tests/CosmonAppKitTests"
        ),
    ]
)
