// SPDX-License-Identifier: MPL-2.0
//
// Four-color palette, copied verbatim from the Verdict app charter
// (apps/Verdict/App/Palette.swift in mailroom). Cadmium is the
// only attention color — every other accent is a refinement of bone,
// charcoal or indigo. Per jr: if there are two attention colors,
// there are none.

import SwiftUI

enum CosmonPalette {
    /// Charcoal #000000 — primary text, separators, dot ink.
    static let charcoal = Color(red: 0.00, green: 0.00, blue: 0.00)
    /// Bone #F4F1E8 — page surface.
    static let bone     = Color(red: 0.957, green: 0.945, blue: 0.910)
    /// Cadmium #D6412B — attention. Use sparingly.
    static let cadmium  = Color(red: 0.839, green: 0.255, blue: 0.169)
    /// Indigo #1A1D4A — discrete state indicator.
    static let indigo   = Color(red: 0.102, green: 0.114, blue: 0.290)

    /// Subtle wash for inert backgrounds (briefing scrollers).
    static let boneShade = Color(red: 0.91, green: 0.90, blue: 0.86)

    /// Four-color status palette — leave callers to map status → color.
    static func status(_ s: String) -> Color {
        switch s {
        case "running":   return indigo
        case "pending":   return cadmium.opacity(0.85)
        case "completed": return charcoal.opacity(0.6)
        case "collapsed": return cadmium
        default:          return charcoal.opacity(0.4)
        }
    }
}
