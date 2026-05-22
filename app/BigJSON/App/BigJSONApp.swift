import SwiftUI
import AppKit

/// Global notification names used to wire menu commands to deep view
/// state without threading bindings through every view layer.
extension Notification.Name {
    /// ⌘F: focus the query bar.
    static let bigJSONFocusQuery = Notification.Name("BigJSON.focusQuery")
    /// ⌘G / ⇧⌘G: step through the current results table.
    /// `userInfo["direction"]: Int` — +1 = next, -1 = previous.
    static let bigJSONStepResult = Notification.Name("BigJSON.stepResult")
}

private let aboutOptions: [NSApplication.AboutPanelOptionKey: Any] = [
    .applicationName: "BigJSON",
    .applicationVersion: "1.0",
    .version: "engine \(Engine.version)",
    .credits: NSAttributedString(
        string: "A native macOS JSON explorer for very large files. "
            + "Apple Silicon, sandbox-aware, mmap-backed.",
        attributes: [
            .font: NSFont.systemFont(ofSize: 11),
            .foregroundColor: NSColor.secondaryLabelColor,
        ]
    ),
]

@main
struct BigJSONApp: App {
    @State private var store = DocumentStore()
    @State private var recents = RecentFilesStore()
    @AppStorage("appTheme") private var appThemeRaw: String = AppTheme.system.rawValue

    private var appTheme: AppTheme {
        AppTheme(rawValue: appThemeRaw) ?? .system
    }

    private var preferredScheme: ColorScheme? {
        switch appTheme {
        case .system: nil
        case .light:  .light
        case .dark:   .dark
        }
    }

    var body: some Scene {
        WindowGroup {
            ContentView(store: store, recents: recents)
                .preferredColorScheme(preferredScheme)
                .onOpenURL { url in
                    store.open(url)
                }
                .onChange(of: store.state) { _, newState in
                    if case .loaded(let doc) = newState {
                        recents.record(url: doc.url)
                    }
                }
        }
        .defaultSize(width: 1280, height: 820)
        .windowResizability(.contentMinSize)
        .commands {
            CommandGroup(replacing: .appInfo) {
                Button("About BigJSON") {
                    NSApplication.shared.orderFrontStandardAboutPanel(options: aboutOptions)
                }
            }
            CommandGroup(replacing: .newItem) {
                Button("Open…") {
                    store.showOpenPanel()
                }
                .keyboardShortcut("o", modifiers: .command)

                Menu("Open Recent") {
                    if recents.entries.isEmpty {
                        Button("(No Recent Items)") {}
                            .disabled(true)
                    } else {
                        ForEach(recents.entries) { entry in
                            Button(entry.displayName) {
                                if let url = recents.resolve(entry) {
                                    store.open(url)
                                }
                            }
                        }
                        Divider()
                        Button("Clear Menu") { recents.clear() }
                    }
                }
            }

            // Find / Next / Prev — wired to the query bar and results
            // table via NotificationCenter so the shortcuts work no
            // matter which control currently has focus.
            CommandGroup(after: .pasteboard) {
                Button("Find") {
                    NotificationCenter.default.post(name: .bigJSONFocusQuery, object: nil)
                }
                .keyboardShortcut("f", modifiers: .command)

                Button("Find Next") {
                    NotificationCenter.default.post(
                        name: .bigJSONStepResult,
                        object: nil,
                        userInfo: ["direction": 1]
                    )
                }
                .keyboardShortcut("g", modifiers: .command)

                Button("Find Previous") {
                    NotificationCenter.default.post(
                        name: .bigJSONStepResult,
                        object: nil,
                        userInfo: ["direction": -1]
                    )
                }
                .keyboardShortcut("g", modifiers: [.command, .shift])
            }

            CommandGroup(after: .toolbar) {
                Menu("Appearance") {
                    ForEach(AppTheme.allCases, id: \.self) { theme in
                        Button {
                            appThemeRaw = theme.rawValue
                        } label: {
                            if appTheme == theme {
                                Label(theme.label, systemImage: "checkmark")
                            } else {
                                Text(theme.label)
                            }
                        }
                    }
                }
            }
        }
    }

}
