import SwiftUI

import NiumaTermCore

struct AgentSessionView: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    @Bindable var model: AgentSessionModel
    let hostName: String

    @FocusState private var composerFocused: Bool
    @State private var renaming = false
    @State private var newTitle = ""
    @State private var confirmingNew = false

    private var linkText: String? { app.host(model.route.hostID)?.linkText }

    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 14) {
                if !model.attached && app.host(model.route.hostID)?.status == .unreachable {
                    VStack(spacing: 10) {
                        Text("Could not connect to \(hostName).").foregroundStyle(Theme.attention)

                        Button("Retry") { app.retry(model.route.hostID) }
                            .buttonStyle(.bordered)
                    }
                    .font(.system(size: 15))
                    .frame(maxWidth: .infinity)
                    .padding(.top, 120)
                } else if !model.attached {
                    HStack(spacing: 10) {
                        ProgressView()
                        Text("Connecting to \(hostName)…")
                    }
                    .font(.system(size: 15))
                    .foregroundStyle(Theme.secondary)
                    .frame(maxWidth: .infinity)
                    .padding(.top, 120)
                } else if model.rows.isEmpty && !model.isWorking {
                    Text("Send a message to start.")
                        .font(.system(size: 15))
                        .foregroundStyle(Theme.tertiary)
                        .frame(maxWidth: .infinity)
                        .padding(.top, 120)
                }

                ForEach(model.rows) { row in
                    TranscriptRow(row: row)
                }

                if model.approval.map({ !$0.submitted }) == true && !model.showApproval {
                    Button { model.showApproval = true } label: {
                        Label("Review approval request", systemImage: "hand.raised")
                            .font(.system(size: 14, weight: .semibold))
                            .foregroundStyle(Theme.attention)
                            .padding(.horizontal, 14)
                            .frame(height: 40)
                            .background(Theme.attention.opacity(0.1), in: .capsule)
                    }
                    .buttonStyle(.plain)
                }

                if model.question != nil && !model.showQuestions {
                    Button { model.showQuestions = true } label: {
                        Label("Answer \(model.agentName)'s question", systemImage: "questionmark.bubble")
                            .font(.system(size: 14, weight: .semibold))
                            .foregroundStyle(Theme.attention)
                            .padding(.horizontal, 14)
                            .frame(height: 40)
                            .background(Theme.attention.opacity(0.1), in: .capsule)
                    }
                    .buttonStyle(.plain)
                }

                if model.isWorking, let started = model.workStarted {
                    WorkingRow(started: started, tokens: model.tokensText)
                }

                if let notice = model.notice {
                    Text(notice)
                        .font(Theme.mono(12.5))
                        .foregroundStyle(Theme.attention)
                }

            }
            .padding(.horizontal, 18)
            .padding(.vertical, 12)
        }
        .defaultScrollAnchor(.bottom, for: .initialOffset)
        .defaultScrollAnchor(.top, for: .sizeChanges)
        .scrollDismissesKeyboard(.interactively)
        .background(Theme.transcriptBackground)
        .safeAreaInset(edge: .bottom) {
            ComposerView(model: model, focused: $composerFocused)
        }
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .principal) {
                VStack(spacing: 1) {
                    Text(model.title).font(.system(size: 16, weight: .semibold)).lineLimit(1)

                    Text([model.profile?.displayName ?? tr("Agent"), hostName, linkText]
                            .compactMap { $0 }.joined(separator: " · "))
                        .font(.system(size: 11.5))
                        .foregroundStyle(Theme.secondary)
                }
            }

            ToolbarItem(placement: .topBarTrailing) {
                Menu {
                    Button("New conversation", systemImage: "square.and.pencil") {
                        confirmingNew = true
                    }
                    .disabled(model.isWorking || model.ended != nil)

                    Button("Rename", systemImage: "pencil") {
                        newTitle = model.title
                        renaming = true
                    }

                    if model.canResume {
                        Button("Recent conversations", systemImage: "clock.arrow.circlepath") {
                            composerFocused = false
                            model.showRecentSessions = true
                        }
                        .disabled(model.ended != nil)
                    }
                } label: {
                    Image(systemName: "ellipsis")
                }
                .disabled(!model.attached)
            }
        }
        .confirmationDialog("Start a new conversation?", isPresented: $confirmingNew, titleVisibility: .visible) {
            Button("New Conversation", role: .destructive) { model.newConversation() }
        } message: {
            Text("\(model.agentName) restarts on \(hostName) with an empty conversation.")
        }
        .alert("Rename session", isPresented: $renaming) {
            TextField("Title", text: $newTitle)
            Button("Cancel", role: .cancel) {}
            Button("Save") { model.rename(newTitle) }
        }
        .sheet(isPresented: $model.showQuestions) {
            if let batch = model.question {
                QuestionSheet(model: model, batch: batch)
                    .presentationDetents([.medium, .large])
            }
        }
        .sheet(isPresented: $model.showApproval) {
            if let approval = model.approval {
                ApprovalSheet(model: model, approval: approval)
                    .presentationDetents([.medium, .large])
            }
        }
        .sheet(isPresented: $model.showRecentSessions) {
            RecentSessionsSheet(model: model)
                .presentationDetents([.medium, .large])
        }
        .sheet(item: Binding(get: { model.ended.map(EndedSheetItem.init) }, set: { _ in })) { item in
            EndedSheet(end: item.end, hostName: hostName, title: model.title,
                       onReconnect: { model.takeControl() },
                       onClose: {
                           Task {
                               try? await Task.sleep(for: .milliseconds(300))

                               dismiss()
                           }
                       })
                .presentationDetents([.height(400)])
                .interactiveDismissDisabled()
        }
    }
}

// MARK: Transcript rows

struct TranscriptRow: View {
    let row: TranscriptRowModel
    @AppStorage("transcriptMono") private var mono = true

    var body: some View {
        switch row.kind {
        case .user(let text, let images):
            VStack(alignment: .trailing, spacing: 4) {
                if images > 0 {
                    Label("\(images) images", systemImage: "photo")
                        .font(.system(size: 12))
                        .foregroundStyle(Theme.secondary)
                }

                if !text.isEmpty {
                    Text(text)
                        .font(.system(size: 15))
                        .foregroundStyle(Theme.ink)
                        .textSelection(.enabled)
                }
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 10)
            .background(Theme.bubble, in: .rect(cornerRadius: 20))
            .frame(maxWidth: .infinity, alignment: .trailing)
            .padding(.leading, 48)
        case .agent(let text):
            Text(markdown(text))
                .font(mono ? Theme.mono(14) : .system(size: 15))
                .lineSpacing(4)
                .foregroundStyle(Theme.ink)
                .frame(maxWidth: .infinity, alignment: .leading)
                .textSelection(.enabled)
        case .reasoning(let summary):
            DisclosureRowView(label: tr("Thinking"), icon: "brain") {
                Text(summary)
                    .font(.system(size: 13.5))
                    .italic()
                    .foregroundStyle(Theme.secondary)
            }
        case .work(let items):
            DisclosureRowView(label: workLabel(items), icon: nil) {
                VStack(alignment: .leading, spacing: 6) {
                    ForEach(items) { WorkItemRow(item: $0) }
                }
            }
        case .compaction(let summary):
            DisclosureRowView(label: tr("Context compacted"), icon: "arrow.down.right.and.arrow.up.left") {
                Text(summary ?? tr("No summary was reported."))
                    .font(.system(size: 13.5))
                    .foregroundStyle(Theme.secondary)
            }
        case .error(let text):
            Text(text)
                .font(Theme.mono(13))
                .foregroundStyle(Theme.attention)
                .padding(.leading, 12)
                .overlay(alignment: .leading) { Rectangle().fill(Theme.attention).frame(width: 2) }
                .textSelection(.enabled)
        case .notice(let text):
            Text(text)
                .font(Theme.mono(13))
                .foregroundStyle(Theme.secondary)
                .padding(.leading, 12)
                .overlay(alignment: .leading) { Rectangle().fill(Theme.rule).frame(width: 2) }
        }
    }

    private func workLabel(_ items: [WorkItem]) -> String {
        if items.count == 1, let item = items.first { return "\(item.label) · \(item.detail)" }

        return tr("Show work (\(items.count))")
    }

    private func markdown(_ s: String) -> AttributedString {
        (try? AttributedString(markdown: s, options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace)))
            ?? AttributedString(s)
    }
}

struct WorkItemRow: View {
    let item: WorkItem
    @State private var open = false

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Button {
                withAnimation(.snappy) { open.toggle() }
            } label: {
                HStack(spacing: 8) {
                    Text(item.label)
                        .foregroundStyle(Theme.accent)
                        .frame(minWidth: 40, alignment: .leading)

                    Text(item.detail)
                        .foregroundStyle(Theme.ink2)
                        .lineLimit(1)
                        .truncationMode(.middle)

                    Spacer(minLength: 4)

                    stateIcon
                }
                .font(Theme.mono(12))
                .contentShape(.rect)
            }
            .buttonStyle(.plain)
            .disabled(item.output?.isEmpty ?? true)

            if open, let output = item.output {
                ScrollView(.horizontal) {
                    Text(output)
                        .font(Theme.mono(11.5))
                        .foregroundStyle(Theme.codeText)
                        .padding(10)
                        .textSelection(.enabled)
                }
                .frame(maxHeight: 280)
                .background(Theme.codeBackground, in: .rect(cornerRadius: 12))
            }
        }
    }

    @ViewBuilder private var stateIcon: some View {
        switch item.state {
        case .running:
            ProgressView().controlSize(.mini)
        case .done:
            Image(systemName: "checkmark")
                .font(.system(size: 10, weight: .bold))
                .foregroundStyle(Theme.online)
        case .failed:
            Image(systemName: "xmark")
                .font(.system(size: 10, weight: .bold))
                .foregroundStyle(Theme.attention)
        }
    }
}

/// The desktop's left-ruled collapsible row.
struct DisclosureRowView<Content: View>: View {
    let label: String
    let icon: String?
    @ViewBuilder var content: () -> Content
    @State private var open = false

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Button {
                withAnimation(.snappy) { open.toggle() }
            } label: {
                HStack(spacing: 6) {
                    if let icon { Image(systemName: icon).font(.system(size: 12)) }

                    Text(label).lineLimit(1).truncationMode(.middle)

                    Image(systemName: "chevron.right")
                        .font(.system(size: 10, weight: .semibold))
                        .rotationEffect(.degrees(open ? 90 : 0))
                }
                .font(Theme.mono(13))
                .foregroundStyle(Theme.secondary)
                .frame(minHeight: 28)
                .contentShape(.rect)
            }
            .buttonStyle(.plain)

            if open { content() }
        }
        .padding(.leading, 12)
        .overlay(alignment: .leading) { Rectangle().fill(Theme.rule).frame(width: 2) }
    }
}

struct WorkingRow: View {
    let started: Date
    let tokens: String?

    var body: some View {
        TimelineView(.periodic(from: .now, by: 1)) { context in
            let seconds = max(0, Int(context.date.timeIntervalSince(started)))

            HStack(spacing: 8) {
                PulsingDots()

                Text("Working for \(seconds / 60)m \(seconds % 60)s")
                    .foregroundStyle(Theme.secondary)

                if let tokens {
                    Text("· \(tokens) tokens")
                        .fontWeight(.semibold)
                        .foregroundStyle(Theme.ink)
                }
            }
            .font(Theme.mono(12.5))
        }
    }
}

struct PulsingDots: View {
    var body: some View {
        TimelineView(.animation) { context in
            let t = context.date.timeIntervalSinceReferenceDate

            HStack(spacing: 3) {
                ForEach(0..<3, id: \.self) { i in
                    Circle()
                        .fill(Theme.accent)
                        .frame(width: 4, height: 4)
                        .opacity(0.25 + 0.75 * (0.5 + 0.5 * sin((t - Double(i) * 0.2) * .pi * 2 / 1.2)))
                }
            }
        }
    }
}
