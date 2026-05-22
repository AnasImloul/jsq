import SwiftUI

extension JSONNodeType {
    /// Three- or four-letter all-caps tag used in result rows, type chips,
    /// and stats popovers. The visible vocabulary across the app — keep
    /// any future renames in lockstep.
    var shortLabel: String {
        switch self {
        case .object: "OBJ"
        case .array:  "ARR"
        case .string: "STR"
        case .number: "NUM"
        case .bool:   "BOOL"
        case .null:   "NULL"
        }
    }

    /// Primary tint used for badges, type chips, and embedded child-row
    /// previews. `.secondary` for null reads as "absent" against any
    /// background and adapts to light/dark mode.
    var accentColor: Color {
        switch self {
        case .object: .indigo
        case .array:  .teal
        case .string: .blue
        case .number: .purple
        case .bool:   .orange
        case .null:   .secondary
        }
    }
}
