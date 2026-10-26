import Observation
import SwiftUI

import NiumaTermCore

/// One row of the transcript as the screen shows it. Consecutive work
/// items fold into one row, as on the desktop.
struct TranscriptRowModel: Identifiable, Equatable {
    enum Kind: Equatable {
        case user(String, images: Int)
        case agent(String)
        case reasoning(String)
        case work([WorkItem])
        case compaction(String?)
        case error(String)
        case notice(String)
    }

    /// The transcript index of the row's first entry, which stays the same
    /// while entries after it stream in.
    let id: Int

    let kind: Kind
}

struct WorkItem: Identifiable, Equatable {
    enum State: Equatable { case running, done, failed }

    let id: Int
    let label: String
    let detail: String
    let output: String?
    let state: State
}

/// Folds transcript entries into rows. Separate from the model so the
/// grouping is a plain function of the entries.
enum TranscriptRows {
    static func build(_ entries: [AgentEntry]) -> [TranscriptRowModel] {
        var rows: [TranscriptRowModel] = []
        var work: [WorkItem] = []
        var workStart = 0

        func flushWork() {
            if !work.isEmpty {
                rows.append(TranscriptRowModel(id: workStart, kind: .work(work)))

                work = []
            }
        }

        for entry in entries {
            let index = Int(entry.index)

            if let item = workItem(entry.item, index: index) {
                if work.isEmpty { workStart = index }

                work.append(item)

                continue
            }

            flushWork()

            switch entry.item {
            case .user(let text, let images):
                rows.append(TranscriptRowModel(id: index, kind: .user(text, images: Int(images))))
            case .agent(let text):
                if !text.isEmpty { rows.append(TranscriptRowModel(id: index, kind: .agent(text))) }
            case .reasoning(let summary):
                if !summary.isEmpty { rows.append(TranscriptRowModel(id: index, kind: .reasoning(summary))) }
            case .compaction(let summary):
                rows.append(TranscriptRowModel(id: index, kind: .compaction(summary)))
            case .error(let text):
                rows.append(TranscriptRowModel(id: index, kind: .error(text)))
            case .notice(let text):
                rows.append(TranscriptRowModel(id: index, kind: .notice(text)))
            case .command, .fileChange, .tool:
                break
            }
        }

        flushWork()

        return rows
    }

    private static func workItem(_ item: AgentItem, index: Int) -> WorkItem? {
        switch item {
        case .command(let command, let purpose, let output, let status, let exitCode):
            let state: WorkItem.State

            if let exitCode {
                state = exitCode == 0 ? .done : .failed
            } else {
                state = workState(status)
            }

            return WorkItem(id: index, label: tr("Run"), detail: purpose ?? command, output: output, state: state)
        case .fileChange(let paths, let diff, let status):
            return WorkItem(id: index, label: tr("Edit"), detail: paths, output: diff, state: workState(status))
        case .tool(let kind, let title, let output, let status):
            return WorkItem(id: index, label: kind, detail: title, output: output, state: workState(status))
        default:
            return nil
        }
    }

    /// Agents report statuses in their own words; anything that is neither
    /// finished nor failed is still running.
    private static func workState(_ status: String?) -> WorkItem.State {
        guard let status = status?.lowercased() else { return .running }

        if status.contains("fail") || status.contains("error") || status.contains("declin") || status.contains("reject") {
            return .failed
        }

        if status.contains("complet") || status.contains("done") || status.contains("success") || status == "ok" {
            return .done
        }

        return .running
    }
}

/// A row the composer offers while the draft is a bare `/name` or `$name`.
enum ComposerSuggestion: Identifiable {
    case command(SlashCommandRecord)
    case skill(SkillRecord)

    var id: String {
        switch self {
        case .command(let command): "/\(command.name)"
        case .skill(let skill): skill.path
        }
    }

    var label: String {
        switch self {
        case .command(let command): "/\(command.name)"
        case .skill(let skill): skill.token
        }
    }

    var detail: String {
        switch self {
        case .command(let command): command.argumentHint.map { "\($0) · \(command.description)" } ?? command.description
        case .skill(let skill): skill.detail
        }
    }

    var enabled: Bool {
        switch self {
        case .command: true
        case .skill(let skill): skill.enabled
        }
    }
}

enum ComposerSettings {
    /// Mirrors the desktop's Codex skill command compatibility, on by default
    /// there too.
    static let codexSkillsInSlashKey = "codexSkillsInSlash"

    /// Mirrors the desktop's one-question-at-a-time answering, off by default
    /// there too.
    static let questionsOneAtATimeKey = "questionsOneAtATime"
}

extension SkillRecord {
    /// The scope and description, so skills that share a name read apart.
    var detail: String {
        let named = title == name ? description : "\(title) · \(description)"

        return "\(source) · \(named)"
    }
}

/// One agent session on a host, over `AgentHandle` (design doc §8.3). The
/// host keeps the controller; this mirrors its view and sends commands.
@MainActor
@Observable
final class AgentSessionModel {
    let route: SessionRoute
    let profile: AgentProfile?
    var title: String

    private(set) var entries: [AgentEntry] = []
    private(set) var rows: [TranscriptRowModel] = []
    private(set) var state: AgentState?

    /// When the running turn started, on this phone's clock.
    private(set) var workStarted: Date?

    var draft = ""
    var showApproval = false
    var showQuestions = false
    var showRecentSessions = false
    private(set) var requestingRecentSessions = false
    private(set) var resumingSession: String?
    var historyNotice: String?

    /// Answers being composed, per question batch, seeded from what the host
    /// holds when the batch first arrives.
    var answers: [String: [QuestionAnswer]] = [:]

    /// Why the last answer from the question sheet did not go through.
    var questionNotice: String?

    /// When the host skips the shown optional batch on its own, on this
    /// phone's clock.
    private(set) var questionSkipsAt: Date?
    var allowForSession = false

    /// The outcome of the last command when it needs saying.
    var notice: String?

    /// The skill picked for the draft's `$name`, which tells it apart from
    /// skills of the same name in other scopes.
    @ObservationIgnored private var pickedSkill: String?

    @ObservationIgnored private var handle: AgentHandle?
    @ObservationIgnored private var events: AgentEvents?
    @ObservationIgnored private var shownApproval: String?
    @ObservationIgnored private var shownQuestion: String?

    init(core: MobileCore, route: SessionRoute, title: String, profile: AgentProfile?) {
        self.route = route
        self.title = title
        self.profile = profile

        let events = AgentEvents(model: self)

        self.events = events

        do {
            handle = try core.attachAgent(host: route.hostID, session: route.sessionID, observer: events)
        } catch {
            notice = error.displayText
        }
    }

    var agentName: String { profile?.shortName ?? tr("the agent") }
    var attached: Bool { state?.attached ?? false }
    var isWorking: Bool { state?.working ?? false }
    var ended: ViewEnd? { state?.ended }
    var queue: [QueuedMessage] { state?.queue ?? [] }
    var approval: PendingApproval? { state?.approval }

    /// The batch the question sheet shows: the oldest one still waiting.
    var question: QuestionBatch? { state?.questions.first }

    var trimmedDraft: String { draft.trimmingCharacters(in: .whitespacesAndNewlines) }
    var showsStop: Bool { isWorking && trimmedDraft.isEmpty }
    var primaryEnabled: Bool { attached && ended == nil && (isWorking || !trimmedDraft.isEmpty) }

    var commands: [SlashCommandRecord] { state?.commands ?? [] }
    var skills: [SkillRecord] { state?.skills ?? [] }
    var canResume: Bool { commands.contains { $0.name == "resume" } }
    var historyLoading: Bool { requestingRecentSessions || (state?.historyLoading ?? false) }
    var recentSessions: [RecentSession] {
        (state?.recentSessions ?? []).sorted {
            $0.lastActive == $1.lastActive ? $0.id < $1.id : $0.lastActive > $1.lastActive
        }
    }

    /// Commands and skills matching the draft while it is still one bare
    /// `/name` or `$name` token; names starting with the query come first.
    /// `skillsInSlash` also lists `$name` skills under `/`, for people who
    /// reach for one key; picking one still writes its `$name` form, since
    /// that is the only form such a harness invokes.
    func suggestions(skillsInSlash: Bool) -> [ComposerSuggestion] {
        guard let sigil = draft.first, sigil == "/" || sigil == "$",
              !draft.contains(where: \.isWhitespace) else { return [] }

        let query = draft.dropFirst().lowercased()

        var candidates = skills
            .filter { $0.token.first == sigil || (sigil == "/" && skillsInSlash) }
            .map(ComposerSuggestion.skill)

        if sigil == "/" {
            candidates = commands.map(ComposerSuggestion.command) + candidates
        }

        let matching = candidates.filter { query.isEmpty || $0.label.dropFirst().lowercased().contains(query) }
        let leading = matching.filter { $0.label.dropFirst().lowercased().hasPrefix(query) }
        let inner = matching.filter { !$0.label.dropFirst().lowercased().hasPrefix(query) }

        return leading + inner
    }

    func applySuggestion(_ suggestion: ComposerSuggestion) {
        draft = suggestion.label + " "

        if case .skill(let skill) = suggestion { pickedSkill = skill.path }
    }

    /// Put a skill's token in front of what is already typed.
    func insertSkill(_ skill: SkillRecord) {
        let rest = trimmedDraft

        draft = rest.isEmpty ? skill.token + " " : "\(skill.token) \(rest)"
        pickedSkill = skill.path
    }

    var model: String? { state?.model }
    var effort: String? { state?.effort }
    var models: [ModelChoice] { state?.models ?? [] }
    var efforts: [String] { models.first { $0.model == model }?.efforts ?? [] }

    var modelLabel: String {
        guard let model else { return tr("Default model") }

        return models.first { $0.model == model }?.display ?? model
    }

    var tokensText: String? {
        state?.outputTokens.map { Self.compact($0) }
    }

    var contextLine: String? {
        guard let used = state?.contextUsed else { return nil }

        guard let window = state?.contextWindow, window > 0 else { return tr("\(Self.compact(used)) context") }

        let left = max(0, 100 - Int(Double(used) / Double(window) * 100))

        return tr("\(Self.compact(used)) used · \(left)% left")
    }

    var phaseLine: String {
        if let failure = state?.startFailure { return failure }

        switch state?.phase {
        case .starting, nil: return attached ? tr("Starting…") : tr("Connecting…")
        case .running: return state?.detail ?? (isWorking ? tr("Working") : tr("Ready"))
        case .idle: return tr("Ready")
        case .exited: return tr("Agent exited")
        }
    }

    // MARK: Core updates

    func viewChanged(transcriptFrom: UInt32?) {
        guard let handle else { return }

        if let from = transcriptFrom {
            let kept = min(Int(from), entries.count)

            entries = Array(entries.prefix(kept)) + handle.entries(from: UInt32(kept))
            rows = TranscriptRows.build(entries)
        }

        let state = handle.state()

        self.state = state

        if let name = state.title, !name.isEmpty { title = name }

        workStarted = state.workingMs.map { Date().addingTimeInterval(-Double($0) / 1000) }

        // A new approval request opens the sheet once; dismissing it keeps
        // a row in the transcript to reopen it.
        switch state.approval {
        case .some(let approval) where !approval.submitted:
            if shownApproval != approval.description {
                shownApproval = approval.description
                allowForSession = false
                showApproval = true
            }
        default:
            shownApproval = nil
            showApproval = false
        }

        // A new question batch opens its sheet once, unless an approval
        // already holds the screen; the transcript keeps a row to open it.
        answers = answers.filter { id, _ in state.questions.contains { $0.id == id } }

        if let batch = state.questions.first {
            if answers[batch.id] == nil {
                answers[batch.id] = batch.questions.map(\.answer)
            }

            questionSkipsAt = batch.skipsInMs.map { Date().addingTimeInterval(Double($0) / 1000) }

            if shownQuestion != batch.id {
                shownQuestion = batch.id
                questionNotice = nil
                showQuestions = !showApproval
            }
        } else {
            shownQuestion = nil
            showQuestions = false
            questionSkipsAt = nil
        }
    }

    // MARK: Commands

    /// Send, queue behind the running turn, or interrupt.
    func primaryAction() {
        let text = trimmedDraft

        if !text.isEmpty {
            draft = ""

            send(text)
        } else if isWorking {
            interrupt()
        }
    }

    private func send(_ text: String) {
        guard let handle else { return }

        notice = nil

        let skill = pickedSkill

        pickedSkill = nil

        Task {
            do {
                let result = try await handle.submit(text: text, skillPath: skill)

                switch result {
                case .accepted, .commandStarted:
                    return
                case .commandFinished(let message):
                    notice = message

                    return
                case .commandQueued(let count):
                    notice = count == 1 ? tr("The command runs when the turn ends.")
                                        : tr("\(count) commands run when the turn ends.")

                    return
                case .conversationReplaced:
                    conversationReplaced()

                    return
                case .showRecentSessions:
                    showRecentSessions = true

                    return
                case .notReady:
                    notice = tr("\(agentName) is still starting.")
                case .answerPending:
                    notice = tr("An answer to a question is still being sent.")
                case .conversationChanging:
                    notice = tr("The conversation is being rewound or forked.")
                case .commandStarting:
                    notice = tr("A command is starting.")
                case .rejected(let message):
                    notice = message
                }
            } catch {
                notice = error.displayText
            }

            // The message did not go out; give it back.
            if draft.isEmpty { draft = text }
        }
    }

    /// Replace the conversation with a new one; the host refuses while a
    /// turn is running.
    func newConversation() {
        guard let handle else { return }

        notice = nil

        Task {
            do {
                try await handle.newConversation()

                conversationReplaced()
            } catch {
                notice = error.displayText
            }
        }
    }

    /// The host names the new conversation once it has a first message.
    private func conversationReplaced() {
        title = profile?.displayName ?? tr("Agent")
    }

    func loadRecentSessions() async {
        guard let handle, !requestingRecentSessions else { return }

        requestingRecentSessions = true
        historyNotice = nil

        defer { requestingRecentSessions = false }

        do {
            try await handle.requestRecentSessions()

            viewChanged(transcriptFrom: nil)
        } catch {
            historyNotice = error.displayText
        }
    }

    func resume(_ session: RecentSession) async {
        guard let handle, resumingSession == nil, !isWorking else { return }

        resumingSession = session.id
        historyNotice = nil

        defer { resumingSession = nil }

        do {
            try await handle.resumeRecentSession(id: session.id)

            showRecentSessions = false
            title = session.title
        } catch {
            historyNotice = error.displayText
        }
    }

    func interrupt() {
        guard let handle else { return }

        Task {
            do {
                let result = try await handle.interrupt()

                if let restored = result.restoredText, draft.isEmpty {
                    draft = restored
                }
            } catch {
                notice = error.displayText
            }
        }
    }

    func withdraw(_ message: QueuedMessage) {
        guard let handle, let id = message.id else { return }
        Task {
            do {
                if try await !handle.withdraw(id: id) {
                    notice = tr("\(agentName) already read that message.")
                }
            } catch {
                notice = error.displayText
            }
        }
    }

    func approve() {
        respond(allowForSession ? .acceptForSession : .accept)
    }

    func deny() {
        respond(.decline)
    }

    private func respond(_ decision: ApprovalDecision) {
        guard let handle else { return }

        showApproval = false

        Task {
            do {
                let result = try await handle.respondApproval(decision: decision)

                if result == .rejected {
                    notice = tr("\(agentName) did not accept the answer.")
                }
            } catch {
                notice = error.displayText
            }
        }
    }

    // MARK: Questions

    func answer(_ batch: QuestionBatch, _ question: Int) -> QuestionAnswer {
        answers[batch.id]?[safe: question] ?? QuestionAnswer(selected: [], text: nil)
    }

    /// Pick or unpick an option; picking one drops typed text, as on the
    /// computer.
    func toggle(_ batch: QuestionBatch, question: Int, option: Int) {
        guard let item = batch.questions[safe: question] else { return }

        var current = answer(batch, question)

        let pick = UInt32(option)

        if !item.multiSelect {
            current.selected = [pick]
        } else if let index = current.selected.firstIndex(of: pick) {
            current.selected.remove(at: index)
        } else {
            current.selected.append(pick)
        }

        current.text = nil

        store(batch, question, current)
    }

    /// Type an answer in place of the options; clearing it brings them back.
    func setText(_ batch: QuestionBatch, question: Int, text: String) {
        var current = answer(batch, question)

        current.text = text.isEmpty ? nil : text

        store(batch, question, current)
    }

    func isAnswered(_ batch: QuestionBatch, _ question: Int) -> Bool {
        let current = answer(batch, question)

        if let text = current.text { return !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }

        return !current.selected.isEmpty
    }

    func isComplete(_ batch: QuestionBatch) -> Bool {
        batch.questions.indices.allSatisfy { isAnswered(batch, $0) }
    }

    func submitQuestions(_ batch: QuestionBatch) {
        settle(batch) { handle, answers in
            try await handle.answerQuestions(id: batch.id, answers: answers)
        }
    }

    func skipQuestions(_ batch: QuestionBatch) {
        settle(batch) { handle, _ in
            try await handle.skipQuestions(id: batch.id)
        }
    }

    private func store(_ batch: QuestionBatch, _ question: Int, _ answer: QuestionAnswer) {
        var current = answers[batch.id] ?? batch.questions.map(\.answer)

        guard current.indices.contains(question) else { return }

        current[question] = answer
        answers[batch.id] = current
        questionNotice = nil
    }

    private func settle(
        _ batch: QuestionBatch,
        _ send: @escaping (AgentHandle, [QuestionAnswer]) async throws -> AnswerResult
    ) {
        guard let handle else { return }
        let composed = answers[batch.id] ?? batch.questions.map(\.answer)
        questionNotice = nil
        Task {
            do {
                switch try await send(handle, composed) {
                case .settled, .waiting, .ignored:
                    showQuestions = false
                case .incomplete:
                    questionNotice = tr("Answer every question first.")
                case .failed:
                    questionNotice = question?.error ?? tr("The answer did not reach \(agentName).")
                }
            } catch {
                questionNotice = error.displayText
            }
        }
    }

    func selectModel(_ model: String) {
        let efforts = models.first { $0.model == model }?.efforts ?? []
        let effort = effort.flatMap { efforts.contains($0) ? $0 : nil }

        apply(model: model, effort: effort)
    }

    func selectEffort(_ effort: String) {
        guard let model else { return }

        apply(model: model, effort: effort)
    }

    private func apply(model: String, effort: String?) {
        guard let handle else { return }

        Task {
            do {
                try await handle.selectModel(model: model, effort: effort)
            } catch {
                notice = error.displayText
            }
        }
    }

    func rename(_ newTitle: String) {
        guard let handle else { return }

        let previous = title

        title = newTitle

        Task {
            do {
                try await handle.rename(title: newTitle)
            } catch {
                title = previous
                notice = error.displayText
            }
        }
    }

    /// Take the session back after the desktop took it (§8.4).
    func takeControl() {
        handle?.takeControl()
    }

    /// Drop the view, which detaches from the session on the host.
    func detach() {
        handle = nil
    }

    static func compact(_ tokens: UInt64) -> String {
        tokens >= 1000 ? String(format: "%.1fk", Double(tokens) / 1000) : "\(tokens)"
    }
}

private extension Array {
    /// A batch can change under a sheet that still shows its old shape, so
    /// lookups by a question's position must not trap.
    subscript(safe index: Int) -> Element? {
        indices.contains(index) ? self[index] : nil
    }
}
