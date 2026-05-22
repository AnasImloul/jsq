# BigJSON.app

The native macOS UI for the BigJSON engine. Streaming open, virtual rows, filter-as-you-type, exports.

A thin SwiftUI layer over the engine's C ABI — nothing semantic about queries, results, or output formatting lives here. Adding a query feature happens in `../engine/`; this crate consumes the result.

## Build

```sh
# From this directory:
xcodebuild -project BigJSON.xcodeproj -scheme BigJSON -configuration Debug build

# Or open in Xcode and ⌘R:
open BigJSON.xcodeproj

# For a packaged .dmg (ad-hoc signed by default — see scripts/release.sh
# for Developer-ID / notarization options):
../scripts/release.sh
```

The "Build Rust engine" build phase calls `../scripts/build-engine.sh` automatically, so `cargo` is the only out-of-band dependency.

## Layout

```
BigJSON/
  App/        @main entry point, top-level configuration.
  Views/      SwiftUI views (query bar, results table/list, stats popover).
  Models/     Swift data types — DocumentStore, JSONNode, QueryModel, etc.
  Engine/     Swift wrappers around the FFI. Document+*.swift split per concern.
  Utilities/  Formatters, extensions.
  Assets.xcassets
BigJSON.xcodeproj/
Info.plist
icon/
```
