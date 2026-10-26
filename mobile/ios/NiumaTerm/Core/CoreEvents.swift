import Foundation

import NiumaTermCore

/// Receives core callbacks on the core's threads and hands them to the main
/// actor, where every model lives.
final class CoreEvents: CoreObserver, @unchecked Sendable {
    private weak var app: AppModel?

    @MainActor
    init(app: AppModel) {
        self.app = app
    }

    func hostChanged(host: HostRecord) {
        Task { @MainActor [weak app] in app?.hostChanged(host) }
    }

    func sessionsChanged(host: String, sessions: [SessionRecord], offer: HostOffer?) {
        Task { @MainActor [weak app] in app?.sessionsChanged(host: host, sessions: sessions, offer: offer) }
    }

    func sessionsFailed(host: String, message: String) {
        Task { @MainActor [weak app] in app?.sessionsFailed(host: host, message: message) }
    }
}

/// Tells an agent screen its view changed. Callbacks only mark the view
/// dirty; the model pulls what changed on the main actor, so a burst of
/// updates never queues a burst of work there.
final class AgentEvents: AgentObserver, @unchecked Sendable {
    private weak var model: AgentSessionModel?

    @MainActor
    init(model: AgentSessionModel) {
        self.model = model
    }

    func changed(transcriptFrom: UInt32?) {
        Task { @MainActor [weak model] in model?.viewChanged(transcriptFrom: transcriptFrom) }
    }
}

/// Tells a terminal screen its view changed. The core stops calling until
/// the screen reads a frame, and the screen reads at most one per display
/// refresh, so output floods never queue work on the main actor. The model
/// is set after creation because a terminal opened here needs its observer
/// before the model exists.
final class TerminalEvents: TerminalObserver, @unchecked Sendable {
    @MainActor weak var model: TerminalSessionModel?

    func changed() {
        Task { @MainActor [weak self] in self?.model?.viewChanged() }
    }
}
