import SwiftUI

import NiumaTermCore

struct RecentSessionsSheet: View {
    @Bindable var model: AgentSessionModel
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            List {
                if model.historyLoading {
                    HStack(spacing: 10) {
                        ProgressView()
                        Text("Loading conversations…")
                    }
                }

                if let notice = model.historyNotice {
                    Text(notice).foregroundStyle(Theme.attention)
                }

                if model.isWorking {
                    Text("Wait for the agent to finish before resuming a conversation.")
                        .foregroundStyle(Theme.secondary)
                }

                ForEach(model.recentSessions, id: \.id) { session in
                    Button {
                        Task { await model.resume(session) }
                    } label: {
                        HStack(spacing: 12) {
                            VStack(alignment: .leading, spacing: 4) {
                                Text(session.title.isEmpty ? session.id : session.title)
                                    .foregroundStyle(Theme.ink)
                                    .lineLimit(2)

                                if let cwd = session.cwd {
                                    Text(cwd)
                                        .font(.caption)
                                        .foregroundStyle(Theme.secondary)
                                        .lineLimit(1)
                                }

                                Text(Date(timeIntervalSince1970: Double(session.lastActive)), style: .relative)
                                    .font(.caption)
                                    .foregroundStyle(Theme.secondary)
                            }

                            Spacer()

                            if model.resumingSession == session.id { ProgressView() }
                        }
                    }
                    .disabled(model.historyLoading || model.resumingSession != nil || model.isWorking
                              || !model.attached || model.ended != nil)
                }
            }
            .overlay {
                if !model.historyLoading && model.historyNotice == nil && model.recentSessions.isEmpty {
                    ContentUnavailableView("No recent conversations", systemImage: "clock.arrow.circlepath",
                                           description: Text("Conversations from this working directory appear here."))
                }
            }
            .navigationTitle(tr("Recent conversations"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Done") { dismiss() }
                }

                ToolbarItem(placement: .topBarTrailing) {
                    Button("Refresh", systemImage: "arrow.clockwise") {
                        Task { await model.loadRecentSessions() }
                    }
                    .disabled(model.historyLoading || model.resumingSession != nil || !model.attached)
                }
            }
            .task { await model.loadRecentSessions() }
        }
        .interactiveDismissDisabled(model.resumingSession != nil)
    }
}
