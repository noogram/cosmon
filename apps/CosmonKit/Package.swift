// swift-tools-version: 5.9
// SPDX-License-Identifier: Apache-2.0

import PackageDescription

// CosmonKit — shared Swift primitives for every cosmon surface.
//
// Hosts `WheatPasteView`, the sole SwiftUI entry point authorised by
// §8k' (cross-surface wheat-paste, ADR-066). Every cosmon-facing
// SwiftUI app (mac-pilot, ios-pilot, Souffleur, Skylight, future
// Vision / Apple TV / e-ink viewports) consumes cosmon state through
// this adapter and no other — a viewport over the `cs peek --snapshot`
// byte raster, never a re-rendering.

let package = Package(
    name: "CosmonKit",
    platforms: [
        .macOS(.v13),
        .iOS(.v16),
    ],
    products: [
        .library(
            name: "CosmonKit",
            targets: ["CosmonKit"]
        ),
    ],
    targets: [
        .target(
            name: "CosmonKit",
            path: "Sources/CosmonKit"
        ),
        .testTarget(
            name: "CosmonKitTests",
            dependencies: ["CosmonKit"],
            path: "Tests/CosmonKitTests"
        ),
    ]
)
