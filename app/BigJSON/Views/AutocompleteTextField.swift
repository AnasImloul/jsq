import SwiftUI
import AppKit

/// Multi-line plain (no-bezel) text view wrapped for SwiftUI, exposing the
/// caret position so callers can run cursor-aware autocomplete and
/// intercepting the keys an autocomplete popup needs (Tab, Return, Up,
/// Down, Esc).
///
/// Enter inserts a newline by default; the popup-driven `onKey` can opt
/// to consume it (when the popup is open) to accept a suggestion instead.
///
/// `text` and `cursor` flow up from edits; setting them from SwiftUI also
/// applies (the cursor only after a programmatic text change, so we don't
/// fight the user's caret while they type).
struct AutocompleteTextField: NSViewRepresentable {
    @Binding var text: String
    @Binding var cursor: Int          // UTF-16 offset (NSRange location)
    @Binding var isFocused: Bool
    var placeholder: String
    var font: NSFont
    var onKey: (KeyAction) -> Bool    // return true to consume
    /// Fires when the user clicks anywhere in the field's window outside
    /// the field's own frame. Used by the autocomplete popup to dismiss
    /// itself even when SwiftUI buttons (which don't take first responder)
    /// receive the click.
    var onClickOutside: (() -> Void)? = nil
    /// Bumped by the parent (e.g. via a ⌘F notification) to grab focus.
    /// updateNSView calls `makeFirstResponder` on each new value; the
    /// initial value is ignored (we don't yank focus on every redraw).
    var focusToken: UUID? = nil

    enum KeyAction {
        case tab
        case enter
        case escape
        case arrowUp
        case arrowDown
    }

    final class Coordinator: NSObject, NSTextViewDelegate {
        var parent: AutocompleteTextField
        var inProgrammaticUpdate = false
        weak var textView: PlaceholderTextView?
        var clickMonitor: Any?
        /// Last seen focusToken, for change detection in updateNSView.
        var lastFocusToken: UUID? = nil

        init(_ parent: AutocompleteTextField) {
            self.parent = parent
        }

        deinit {
            if let m = clickMonitor {
                NSEvent.removeMonitor(m)
            }
        }

        func installClickOutsideMonitor(view: NSView) {
            // App-local left-mouse-down monitor. Dismisses the popup when
            // the click is in the field's window but outside the field's
            // frame. Dispatches the callback async so the click still
            // reaches its target view (popup row, button, etc.) before
            // the popup tears down.
            clickMonitor = NSEvent.addLocalMonitorForEvents(matching: .leftMouseDown) { [weak self] event in
                guard let self, let tv = self.textView else { return event }
                guard let window = tv.window, event.window === window else { return event }
                let locationInView = tv.convert(event.locationInWindow, from: nil)
                if !tv.bounds.contains(locationInView) {
                    let callback = self.parent.onClickOutside
                    DispatchQueue.main.async { callback?() }
                }
                return event
            }
        }

        func textDidChange(_ notification: Notification) {
            if inProgrammaticUpdate { return }
            guard let tv = notification.object as? NSTextView else { return }
            parent.text = tv.string
            parent.cursor = tv.selectedRange().location
        }

        func textViewDidChangeSelection(_ notification: Notification) {
            if inProgrammaticUpdate { return }
            guard let tv = notification.object as? NSTextView else { return }
            parent.cursor = tv.selectedRange().location
        }

        func textView(
            _ textView: NSTextView,
            doCommandBy selector: Selector
        ) -> Bool {
            // Push current cursor up before deciding whether to consume,
            // so handlers see the latest position.
            parent.cursor = textView.selectedRange().location

            let action: KeyAction?
            switch selector {
            case #selector(NSStandardKeyBindingResponding.insertTab(_:)),
                 #selector(NSStandardKeyBindingResponding.insertBacktab(_:)):
                action = .tab
            case #selector(NSStandardKeyBindingResponding.insertNewline(_:)):
                // Shift+Enter inserts a newline unconditionally, even when
                // the autocomplete popup is open — gives the user an escape
                // hatch when Enter would otherwise accept a suggestion.
                let shiftHeld = NSApp.currentEvent?
                    .modifierFlags
                    .contains(.shift) ?? false
                if shiftHeld {
                    return false
                }
                action = .enter
            case #selector(NSStandardKeyBindingResponding.cancelOperation(_:)):
                action = .escape
            case #selector(NSStandardKeyBindingResponding.moveUp(_:)):
                action = .arrowUp
            case #selector(NSStandardKeyBindingResponding.moveDown(_:)):
                action = .arrowDown
            default:
                action = nil
            }
            if let a = action {
                // When the popup handler returns false (popup not open),
                // fall through to the text view's default — Enter inserts
                // a newline, arrows navigate lines, Tab inserts a tab.
                return parent.onKey(a)
            }
            return false
        }
    }

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    func makeNSView(context: Context) -> PlaceholderTextView {
        let tv = PlaceholderTextView()
        tv.delegate = context.coordinator
        tv.font = font
        tv.placeholderString = placeholder
        // Rich text on so the syntax highlighter's per-range attributes
        // are honoured. The `usesRuler` / `usesFontPanel` settings stay
        // off so the user can't accidentally invoke font/colour pickers.
        tv.isRichText = true
        tv.usesFontPanel = false
        tv.usesRuler = false
        // Newly typed characters inherit these attributes — without
        // this the highlighter's last-token colour would bleed onto
        // anything the user types afterwards.
        tv.typingAttributes = [
            .font: font,
            .foregroundColor: NSColor.labelColor,
        ]
        tv.installSyntaxHighlighter()
        tv.allowsUndo = true
        tv.isAutomaticQuoteSubstitutionEnabled = false
        tv.isAutomaticDashSubstitutionEnabled = false
        tv.isAutomaticTextReplacementEnabled = false
        tv.isAutomaticSpellingCorrectionEnabled = false
        tv.isAutomaticLinkDetectionEnabled = false
        tv.isAutomaticDataDetectionEnabled = false
        tv.smartInsertDeleteEnabled = false
        tv.drawsBackground = false
        tv.textContainerInset = NSSize(width: 0, height: 2)

        // Auto-grow plumbing: the text container tracks the view's width
        // (so wrapping happens against the SwiftUI-allocated frame) but
        // NOT its height (so vertical content drives intrinsic size).
        // `maxSize.height = greatestFiniteMagnitude` means "let layout
        // decide how tall I am" — NSTextView consults this when sizing.
        tv.isVerticallyResizable = true
        tv.isHorizontallyResizable = false
        tv.autoresizingMask = [.width]
        tv.minSize = NSSize(width: 0, height: 0)
        tv.maxSize = NSSize(
            width: CGFloat.greatestFiniteMagnitude,
            height: CGFloat.greatestFiniteMagnitude
        )
        if let container = tv.textContainer {
            container.lineFragmentPadding = 0
            container.widthTracksTextView = true
            container.heightTracksTextView = false
            container.containerSize = NSSize(
                width: 0,
                height: CGFloat.greatestFiniteMagnitude
            )
        }
        tv.string = text
        // Initial highlight pass — `tv.string = ...` doesn't trigger
        // `didChangeText`, so without this the saved-on-disk query a
        // document opens with would render unstyled until the user
        // edited it.
        tv.applySyntaxHighlight()

        tv.onFocusChange = { [weak coord = context.coordinator] focused in
            guard let coord = coord else { return }
            DispatchQueue.main.async {
                coord.parent.isFocused = focused
            }
        }

        context.coordinator.textView = tv
        context.coordinator.installClickOutsideMonitor(view: tv)
        return tv
    }

    static func dismantleNSView(
        _ nsView: PlaceholderTextView,
        coordinator: Coordinator
    ) {
        if let m = coordinator.clickMonitor {
            NSEvent.removeMonitor(m)
            coordinator.clickMonitor = nil
        }
    }

    func updateNSView(_ tv: PlaceholderTextView, context: Context) {
        // Update the NSView's coordinator's parent reference so the
        // closure-captures stay in sync with SwiftUI's state.
        context.coordinator.parent = self

        // Programmatic text change: replace and move caret to the bound
        // cursor position. Done in a coordinator-flagged block so we don't
        // forward the resulting textDidChange back into the binding.
        if tv.string != text {
            context.coordinator.inProgrammaticUpdate = true
            tv.string = text
            let length = (text as NSString).length
            let safe = max(0, min(cursor, length))
            tv.setSelectedRange(NSRange(location: safe, length: 0))
            context.coordinator.inProgrammaticUpdate = false
            tv.applySyntaxHighlight()
            tv.invalidateIntrinsicContentSize()
            tv.needsDisplay = true
        }

        if tv.font != font {
            tv.font = font
            // Font change forces a re-highlight: the per-token bold
            // variant is derived from the current font and otherwise
            // wouldn't update.
            tv.typingAttributes = [
                .font: font,
                .foregroundColor: NSColor.labelColor,
            ]
            tv.applySyntaxHighlight()
        }
        if tv.placeholderString != placeholder {
            tv.placeholderString = placeholder
            tv.needsDisplay = true
        }

        // Programmatic focus: when the parent bumps `focusToken` we make
        // ourselves first responder. Skip on initial materialisation.
        if let token = focusToken, token != context.coordinator.lastFocusToken {
            context.coordinator.lastFocusToken = token
            DispatchQueue.main.async {
                tv.window?.makeFirstResponder(tv)
                let len = (tv.string as NSString).length
                tv.setSelectedRange(NSRange(location: len, length: 0))
            }
        }
    }
}

/// `NSTextView` subclass with a drawn placeholder and an intrinsic content
/// size that grows with the text — so multi-line queries expand the field
/// vertically inside SwiftUI's HStack.
final class PlaceholderTextView: NSTextView {
    var placeholderString: String = "" {
        didSet { needsDisplay = true }
    }
    var onFocusChange: ((Bool) -> Void)?

    override var intrinsicContentSize: NSSize {
        guard let lm = layoutManager, let tc = textContainer else {
            return super.intrinsicContentSize
        }
        let lineHeight = (font ?? NSFont.systemFont(ofSize: NSFont.systemFontSize))
            .boundingRectForFont.height
        // Until we have a non-zero frame width, the layout manager would
        // wrap every word onto its own line and report a huge height.
        // Fall back to a single-line height; we'll recompute as soon as
        // setFrameSize gives us a real width.
        if bounds.width <= 1 {
            return NSSize(
                width: NSView.noIntrinsicMetric,
                height: ceil(lineHeight + textContainerInset.height * 2)
            )
        }
        lm.ensureLayout(for: tc)
        let used = lm.usedRect(for: tc)
        let height = max(used.height, lineHeight) + textContainerInset.height * 2
        return NSSize(width: NSView.noIntrinsicMetric, height: ceil(height))
    }

    override func setFrameSize(_ newSize: NSSize) {
        let widthChanged = newSize.width != bounds.width
        super.setFrameSize(newSize)
        if widthChanged {
            // Width drives wrapping → wrapping drives our intrinsic
            // height. Force SwiftUI to re-query.
            invalidateIntrinsicContentSize()
        }
    }

    override func didChangeText() {
        super.didChangeText()
        applySyntaxHighlight()
        invalidateIntrinsicContentSize()
        needsDisplay = true
    }

    /// Installs the syntax-highlighter machinery. Idempotent — safe to
    /// call from `makeNSView` even if Cocoa has already initialised the
    /// text storage. Doesn't run an initial highlight pass; callers do
    /// that after they've set the field's content.
    func installSyntaxHighlighter() {
        // Nothing to wire as a delegate — `didChangeText` is the
        // single hook we need. Method exists so the call site reads
        // intentionally and so we have a place to add lazier behaviour
        // (debouncing, incremental retokenisation) later if it's ever
        // worth doing.
    }

    /// Re-applies token colours to the entire text storage. Cheap for
    /// the query-bar shape (typical input is short); we don't bother
    /// with incremental updates. Also resets `typingAttributes` so the
    /// next character the user types starts with default colour rather
    /// than inheriting whichever token sat next to the caret.
    func applySyntaxHighlight() {
        guard let storage = textStorage else { return }
        let f = font ?? NSFont.systemFont(ofSize: NSFont.systemFontSize)
        QuerySyntaxHighlighter.highlight(storage, font: f)
        typingAttributes = [
            .font: f,
            .foregroundColor: NSColor.labelColor,
        ]
    }

    override func draw(_ dirtyRect: NSRect) {
        super.draw(dirtyRect)
        guard string.isEmpty, !placeholderString.isEmpty else { return }
        let attrs: [NSAttributedString.Key: Any] = [
            .font: font ?? NSFont.systemFont(ofSize: NSFont.systemFontSize),
            .foregroundColor: NSColor.placeholderTextColor
        ]
        let origin = NSPoint(
            x: textContainerOrigin.x,
            y: textContainerOrigin.y
        )
        (placeholderString as NSString).draw(at: origin, withAttributes: attrs)
    }

    override func becomeFirstResponder() -> Bool {
        let r = super.becomeFirstResponder()
        if r { onFocusChange?(true) }
        return r
    }

    override func resignFirstResponder() -> Bool {
        let r = super.resignFirstResponder()
        if r { onFocusChange?(false) }
        return r
    }
}
