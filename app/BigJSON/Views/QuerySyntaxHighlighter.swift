import AppKit

/// Applies the engine's tokenizer output to an NSTextStorage. Resets to
/// a base label-coloured run first so deletions don't leave orphaned
/// colour. The grammar — which strings are keywords, what counts as a
/// reducer, etc. — lives in Rust (`engine/src/query/grammar.rs`); this
/// file only maps a `TokenCategory` to a colour.
enum QuerySyntaxHighlighter {
    static func highlight(_ storage: NSTextStorage, font: NSFont) {
        let str = storage.string
        let nsstr = str as NSString
        let full = NSRange(location: 0, length: nsstr.length)

        let baseAttrs: [NSAttributedString.Key: Any] = [
            .font: font,
            .foregroundColor: NSColor.labelColor,
        ]

        storage.beginEditing()
        storage.setAttributes(baseAttrs, range: full)
        for token in Engine.tokenize(str) {
            let attrs = self.attributes(for: token.category, font: font)
            // Tokens come from Rust with UTF-16 offsets, so they slot
            // straight into NSTextStorage without conversion.
            storage.addAttributes(attrs, range: token.nsRange)
        }
        storage.endEditing()
    }

    private static func attributes(
        for category: Engine.TokenCategory,
        font: NSFont
    ) -> [NSAttributedString.Key: Any] {
        switch category {
        case .keyword:
            return [.foregroundColor: keywordColor, .font: bold(font)]
        case .reducer:
            return [.foregroundColor: reducerColor, .font: bold(font)]
        case .literal:
            return [.foregroundColor: literalColor, .font: bold(font)]
        case .string:
            return [.foregroundColor: stringColor]
        case .number:
            return [.foregroundColor: numberColor]
        case .comment:
            return [.foregroundColor: commentColor, .obliqueness: 0.18]
        case .operator:
            return [.foregroundColor: operatorColor]
        case .splat:
            return [.foregroundColor: splatColor, .font: bold(font)]
        case .punctuation:
            return [.foregroundColor: punctuationColor]
        case .identifier, .error:
            return [.foregroundColor: NSColor.labelColor]
        }
    }

    /// Bold variant of `font` if the manager can produce one; otherwise
    /// the input. Lets keyword highlighting stay readable when the user
    /// has picked a non-default font.
    private static func bold(_ font: NSFont) -> NSFont {
        let manager = NSFontManager.shared
        return manager.convert(font, toHaveTrait: .boldFontMask)
    }

    // SQL-IDE-ish palette. All colours are dynamic system colours so
    // they adapt across light/dark appearances.
    private static let keywordColor = NSColor.systemPurple
    private static let reducerColor = NSColor.systemPurple
    private static let literalColor = NSColor.systemPurple
    private static let stringColor = NSColor.systemRed
    private static let numberColor = NSColor.systemBlue
    private static let commentColor = NSColor.secondaryLabelColor
    private static let operatorColor = NSColor.secondaryLabelColor
    private static let splatColor = NSColor.systemOrange
    private static let punctuationColor = NSColor.tertiaryLabelColor
}
