import Foundation
import SwiftUI
import WidgetKit

private struct StatusEntry: TimelineEntry {
    let date: Date
}

private struct StatusProvider: TimelineProvider {
    func placeholder(in context: Context) -> StatusEntry {
        StatusEntry(date: Date())
    }

    func getSnapshot(
        in context: Context,
        completion: @escaping (StatusEntry) -> Void
    ) {
        completion(StatusEntry(date: Date()))
    }

    func getTimeline(
        in context: Context,
        completion: @escaping (Timeline<StatusEntry>) -> Void
    ) {
        completion(Timeline(
            entries: [StatusEntry(date: Date())],
            policy: .never
        ))
    }
}

@main
struct ErNoteWidget: Widget {
    var body: some WidgetConfiguration {
        StaticConfiguration(
            kind: "ErNoteStatus",
            provider: StatusProvider()
        ) { _ in
            VStack(alignment: .leading, spacing: 8) {
                Text("ER Note").font(.headline)
                Text("Open the iPhone app.")
                Text("Patient status is unavailable.").font(.caption)
            }
            .containerBackground(.fill.tertiary, for: .widget)
        }
        .configurationDisplayName("ER Note")
        .description("Open ER Note on your iPhone.")
        .supportedFamilies([.systemSmall])
    }
}
