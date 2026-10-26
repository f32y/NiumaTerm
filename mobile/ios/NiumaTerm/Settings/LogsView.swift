import SwiftUI
import UIKit

import NiumaTermCore

struct LogsView: View {
    @Environment(AppModel.self) private var app
    @State private var text = ""
    @State private var confirmingClear = false

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                Text("Connection and core diagnostics from this app launch.")
                    .font(.caption)
                    .foregroundStyle(Theme.secondary)

                if let error = app.startupError {
                    Text(error).foregroundStyle(Theme.attention)
                }

                if text.isEmpty {
                    Text("No logs yet.").foregroundStyle(Theme.secondary)
                } else {
                    Text(text)
                        .font(Theme.mono(11))
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
            .padding()
        }
        .navigationTitle(tr("Logs"))
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Menu {
                    Button("Refresh", systemImage: "arrow.clockwise") { refresh() }

                    Button("Copy", systemImage: "doc.on.doc") { UIPasteboard.general.string = text }
                        .disabled(text.isEmpty)

                    Button("Clear logs", systemImage: "trash", role: .destructive) { confirmingClear = true }
                        .disabled(text.isEmpty)
                } label: {
                    Image(systemName: "ellipsis.circle")
                }
            }
        }
        .alert("Clear logs?", isPresented: $confirmingClear) {
            Button("Cancel", role: .cancel) {}

            Button("Clear logs", role: .destructive) {
                clearDiagnosticLog()
                refresh()
            }
        }
        .task {
            refresh()

            while !Task.isCancelled {
                do {
                    try await Task.sleep(for: .seconds(1))
                } catch {
                    return
                }

                refresh()
            }
        }
    }

    private func refresh() {
        text = diagnosticLog()
    }
}
