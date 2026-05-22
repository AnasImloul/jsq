import SwiftUI

/// Slim bar across the bottom of the loaded view. Document stats on
/// the left, live selection readout on the right. ~24pt tall — meant
/// to be glanceable, not interactive.
struct StatusBar: View {
    let document: Engine.Document
    let selection: JSONNode.ID?

    var body: some View {
        HStack(spacing: 14) {
            sizeChip
            nodeCountChip
            cacheChip
            memoryChip
            cpuChip
            Spacer(minLength: 12)
            selectionView
        }
        .font(.caption)
        .foregroundStyle(.secondary)
        .padding(.horizontal, 12)
        .padding(.vertical, 4)
        .frame(height: 24)
        .background(.bar)
    }

    private var sizeChip: some View {
        HStack(spacing: 4) {
            Image(systemName: "doc").font(.system(size: 10))
            Text(Formatters.bytes(document.fileSize))
        }
        .help("File size")
    }

    private var nodeCountChip: some View {
        HStack(spacing: 4) {
            Image(systemName: "circle.grid.2x2").font(.system(size: 10))
            Text("\(Formatters.count(document.totalNodeCount)) nodes")
        }
        .help("Total nodes in the document")
    }

    @ViewBuilder
    private var cacheChip: some View {
        if document.loadedFromSidecar {
            HStack(spacing: 4) {
                Image(systemName: "bolt.fill")
                    .foregroundStyle(.green)
                    .font(.system(size: 10))
                Text("Cached")
            }
            .help("Loaded from sidecar index")
        }
    }

    private var memoryChip: some View {
        TimelineView(.periodic(from: .now, by: 2.0)) { _ in
            HStack(spacing: 4) {
                Image(systemName: "memorychip").font(.system(size: 10))
                Text("Mem \(Formatters.bytes(MemoryReporter.residentBytes()))")
            }
        }
        .help("Memory footprint of the app (matches Activity Monitor)")
    }

    private var cpuChip: some View {
        TimelineView(.periodic(from: .now, by: 1.0)) { _ in
            HStack(spacing: 4) {
                Image(systemName: "cpu").font(.system(size: 10))
                Text("CPU \(Formatters.cpuPercent(CPUReporter.shared.samplePercent()))")
            }
        }
        .help("CPU usage averaged over the last second (100% = one core)")
    }

    @ViewBuilder
    private var selectionView: some View {
        if let id = selection,
           let engineID = document.engineNodeID(from: id) {
            let kind = document.kind(of: engineID).toJSONNodeType()
            let path = document.path(of: engineID)
            HStack(spacing: 6) {
                Text(kind.label)
                    .foregroundStyle(.tertiary)
                    .help("Selected node type")
                Text("→")
                    .foregroundStyle(.tertiary)
                Text(path)
                    .font(.system(.caption, design: .monospaced))
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .frame(maxWidth: 320, alignment: .trailing)
                    .help(path)
            }
        }
    }
}
