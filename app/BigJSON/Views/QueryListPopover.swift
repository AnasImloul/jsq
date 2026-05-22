import SwiftUI

/// Popover content that lists queries (recent or saved) full-width
/// with proper line wrapping — long jq filters need the room. Click
/// a row to load the query; hover to reveal a delete affordance.
struct QueryListPopover: View {
    let title: String
    let icon: String
    let entries: [QueryListEntry]
    let onSelect: (QueryListEntry) -> Void
    let onDelete: (QueryListEntry) -> Void
    let onClearAll: (() -> Void)?
    let emptyMessage: String

    @State private var hoveredID: UUID?

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            Divider()
            if entries.isEmpty {
                empty
            } else {
                list
            }
        }
        .frame(minWidth: 460, idealWidth: 520, maxWidth: 720,
               minHeight: 80, idealHeight: 360, maxHeight: 520)
    }

    private var header: some View {
        HStack(spacing: 6) {
            Image(systemName: icon)
                .foregroundStyle(.secondary)
                .font(.callout)
            Text(title)
                .font(.headline)
            Spacer()
            Text("\(entries.count)")
                .font(.caption.monospacedDigit())
                .foregroundStyle(.tertiary)
            if let onClearAll, !entries.isEmpty {
                Button("Clear All", action: onClearAll)
                    .controlSize(.small)
                    .buttonStyle(.borderless)
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 10)
        .background(.bar)
    }

    private var list: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 0) {
                ForEach(entries) { entry in
                    row(entry: entry)
                    Divider().padding(.leading, 12)
                }
            }
        }
    }

    @ViewBuilder
    private func row(entry: QueryListEntry) -> some View {
        let isHovered = hoveredID == entry.id
        Button {
            onSelect(entry)
        } label: {
            HStack(alignment: .top, spacing: 8) {
                VStack(alignment: .leading, spacing: 2) {
                    if let label = entry.label, !label.isEmpty {
                        Text(label)
                            .font(.callout.weight(.semibold))
                            .lineLimit(1)
                    }
                    Text(entry.query)
                        .font(.system(.callout, design: .monospaced))
                        .foregroundStyle(entry.label != nil ? .secondary : .primary)
                        .lineLimit(4)
                        .truncationMode(.tail)
                        .multilineTextAlignment(.leading)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .textSelection(.enabled)
                }
                if isHovered {
                    Button {
                        onDelete(entry)
                    } label: {
                        Image(systemName: "xmark.circle.fill")
                            .font(.callout)
                            .foregroundStyle(.tertiary)
                    }
                    .buttonStyle(.plain)
                    .help(entry.label != nil ? "Unbookmark" : "Remove from history")
                }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .frame(maxWidth: .infinity, alignment: .leading)
            .contentShape(Rectangle())
            .background(isHovered ? Color.accentColor.opacity(0.08) : Color.clear)
        }
        .buttonStyle(.plain)
        .onHover { hovering in
            hoveredID = hovering ? entry.id : (hoveredID == entry.id ? nil : hoveredID)
        }
    }

    private var empty: some View {
        VStack(spacing: 8) {
            Spacer()
            Image(systemName: icon)
                .font(.system(size: 24, weight: .light))
                .foregroundStyle(.tertiary)
            Text(emptyMessage)
                .font(.callout)
                .foregroundStyle(.tertiary)
            Spacer()
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(20)
    }
}

/// Adapter so the popover doesn't have to know about `String` vs the
/// `SavedQueriesStore.Entry` shape — recent queries get a synthetic id.
struct QueryListEntry: Identifiable {
    let id: UUID
    let query: String
    /// Display label shown above the query body (saved queries only).
    let label: String?

    static func recent(_ q: String) -> QueryListEntry {
        // Stable hashed id so SwiftUI can diff the list, but unique per
        // query (we de-dupe before populating, so collisions don't fire).
        let hash = UInt64(bitPattern: Int64(q.hashValue))
        let bytes: [UInt8] = (0..<8).map { UInt8((hash >> (8 * $0)) & 0xFF) }
        let id = UUID(uuid: (
            bytes[0], bytes[1], bytes[2], bytes[3],
            bytes[4], bytes[5], bytes[6], bytes[7],
            0, 0, 0, 0, 0, 0, 0, 0
        ))
        return QueryListEntry(id: id, query: q, label: nil)
    }

    static func saved(_ entry: SavedQueriesStore.Entry) -> QueryListEntry {
        QueryListEntry(id: entry.id, query: entry.query, label: entry.name)
    }
}
