import Network
import Observation
import SwiftUI
import UIKit
import UserNotifications
import os

import NiumaTermCore

/// App-wide state over `MobileCore` (design doc §4.2): paired hosts, their
/// live session lists, and the open agent and terminal views.
@MainActor
@Observable
final class AppModel {
    var hosts: [Host] = []
    var path = NavigationPath()

    /// Why the core could not start, which leaves the app unable to pair.
    var startupError: String?

    @ObservationIgnored private var core: MobileCore?
    @ObservationIgnored private var events: CoreEvents?
    @ObservationIgnored private var agentModels: [String: AgentSessionModel] = [:]
    @ObservationIgnored private var terminalModels: [String: TerminalSessionModel] = [:]

    /// Tells the core when the phone's network changes, which iOS reports
    /// only to the app.
    @ObservationIgnored private let pathMonitor = NWPathMonitor()

    /// The APNs device token, once iOS issued one.
    @ObservationIgnored private var pushToken: String?

    @ObservationIgnored private let pushLog = Logger(subsystem: "io.f32.NiumaTermMobile", category: "push")

    init() {
        do {
            let core = try MobileCore(stateDir: Self.stateDirectory().path,
                                      deviceName: UIDevice.current.name,
                                      appVersion: Self.appVersion)

            self.core = core

            // Before anything connects, so no link starts on a path the
            // user ruled out.
            core.setNetworkMode(mode: NetworkPreference.stored.mode)

            hosts = core.hosts().map(Host.init(record:))

            let events = CoreEvents(app: self)

            self.events = events

            core.observe(observer: events)

            // Joining another Wi-Fi network may put the phone on a host's
            // LAN: links through a relay then try going direct at once, and
            // every link checks it still reaches its host.
            pathMonitor.pathUpdateHandler = { _ in core.networkChanged() }

            pathMonitor.start(queue: DispatchQueue(label: "io.f32.NiumaTermMobile.network"))
        } catch {
            startupError = error.displayText
        }
    }

    static var appVersion: String {
        Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "0"
    }

    /// Pairing records and the sealed device key live in Application Support,
    /// which is backed up but never shown to the user.
    private static func stateDirectory() throws -> URL {
        let base = try FileManager.default.url(for: .applicationSupportDirectory, in: .userDomainMask,
                                               appropriateFor: nil, create: true)

        let dir = base.appendingPathComponent("remote", isDirectory: true)

        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)

        return dir
    }

    // MARK: Lifecycle

    func setForeground(_ foreground: Bool) {
        core?.setForeground(foreground: foreground)
    }

    // MARK: Core events

    func hostChanged(_ record: HostRecord) {
        let wasConnected = host(record.id)?.isOnline ?? false

        if let index = hosts.firstIndex(where: { $0.id == record.id }) {
            hosts[index].name = record.name
            hosts[index].status = record.status
            hosts[index].link = record.link

            // A list from a dropped link may no longer be true, and its rows
            // could not be opened anyway; the host lists its sessions again
            // once the link is back.
            if record.status != .connected {
                hosts[index].sessions = []
                hosts[index].offer = nil
                hosts[index].sessionsLoaded = false
                hosts[index].sessionLoadError = nil
            }
        } else if core?.hosts().contains(where: { $0.id == record.id }) == true {
            // Events reach the main actor asynchronously, so one sent just
            // before a host was forgotten can arrive after it; only hosts
            // the core still pairs with may be added back.
            hosts.append(Host(record: record))
        }

        // A host keeps the registration, but one paired before pushes
        // existed, or that dropped a token APNs refused, needs it again.
        if record.status == .connected && !wasConnected {
            registerPush(record.id)
        }
    }

    func sessionsChanged(host: String, sessions: [SessionRecord], offer: HostOffer?) {
        guard let index = hosts.firstIndex(where: { $0.id == host }), hosts[index].isOnline else { return }

        hosts[index].sessions = sessions.map { Session(record: $0, hostID: host) }
        hosts[index].offer = offer
        hosts[index].sessionsLoaded = true
        hosts[index].sessionLoadError = nil
    }

    func sessionsFailed(host: String, message: String) {
        guard let index = hosts.firstIndex(where: { $0.id == host }), hosts[index].isOnline else { return }

        hosts[index].sessionsLoaded = false
        hosts[index].sessionLoadError = message
    }

    // MARK: Lookups

    func host(_ id: String) -> Host? {
        hosts.first { $0.id == id }
    }

    func session(_ route: SessionRoute) -> Session? {
        host(route.hostID)?.sessions.first { $0.id == route.sessionID }
    }

    /// Whether the agent screen for `session` is open. Its model exists
    /// exactly while the screen is shown, since leaving the screen closes it.
    func isShowingAgent(host: String, session: String) -> Bool {
        agentModels["\(host)/\(session)"] != nil
    }

    // MARK: Commands

    func pair(_ input: String, relayURL: String? = nil, accessKey: String? = nil) async throws -> HostRecord {
        guard let core else { throw CoreError.Failed(message: startupError ?? tr("The app could not start its core.")) }

        let record = try await core.pair(input: input, relayUrl: relayURL, accessKey: accessKey)

        hostChanged(record)
        Task { await startPush() }

        return record
    }

    /// The core withdraws pushes from the host in the background, if it is
    /// connected, and drops the host at once, so an unreachable host is
    /// forgotten without waiting on it.
    func forget(_ hostID: String) {
        core?.forget(host: hostID)
        PushKeys.remove(for: hostID)
        hosts.removeAll { $0.id == hostID }

        for key in agentModels.keys where key.hasPrefix(hostID + "/") {
            agentModels[key] = nil
        }

        for key in terminalModels.keys where key.hasPrefix(hostID + "/") {
            terminalModels[key] = nil
        }
    }

    /// Try a host that failed to connect again.
    func retry(_ hostID: String) {
        core?.retry(host: hostID)
    }

    func retrySessions(_ hostID: String) {
        guard let core, let index = hosts.firstIndex(where: { $0.id == hostID }), hosts[index].isOnline else { return }

        hosts[index].sessionLoadError = nil

        Task {
            do {
                try await core.refreshSessions(host: hostID)
            } catch {
                sessionsFailed(host: hostID, message: error.displayText)
            }
        }
    }

    func hostOffer(_ hostID: String) async throws -> HostOffer {
        guard let core else { throw CoreError.Failed(message: tr("The app could not start its core.")) }

        return try await core.hostInfo(host: hostID)
    }

    /// `agent.open`; returns the route of the new session.
    func openAgent(hostID: String, profile: String, workspace: String) async throws -> SessionRoute {
        guard let core else { throw CoreError.Failed(message: tr("The app could not start its core.")) }

        let session = try await core.openAgent(host: hostID, profile: profile, workspace: workspace)

        return SessionRoute(hostID: hostID, sessionID: session, kind: .agent)
    }

    /// Attaching takes control of the session from the desktop (§8.4). The
    /// model stays cached while the screen is open, so the view survives
    /// navigation redraws; `closeAgent` detaches.
    func agentModel(for route: SessionRoute, session: Session?) -> AgentSessionModel? {
        let key = "\(route.hostID)/\(route.sessionID)"

        if let model = agentModels[key] { return model }

        guard let core else { return nil }

        let model = AgentSessionModel(core: core, route: route,
                                      title: session?.title ?? tr("Agent"),
                                      profile: session?.profile)

        agentModels[key] = model

        return model
    }

    /// Drop the view of a session, which detaches from it on the host.
    func closeAgent(_ route: SessionRoute) {
        agentModels["\(route.hostID)/\(route.sessionID)"]?.detach()

        agentModels["\(route.hostID)/\(route.sessionID)"] = nil
    }

    /// Start a shell on the host and return the route of its screen, whose
    /// view is already attached.
    func openTerminal(hostID: String) async throws -> SessionRoute {
        guard let core else { throw CoreError.Failed(message: tr("The app could not start its core.")) }

        let grid = TerminalMetrics.estimatedGrid()
        let events = TerminalEvents()

        let handle = try await core.openTerminal(host: hostID, cols: UInt16(grid.cols), rows: UInt16(grid.rows),
                                                 observer: events)

        let route = SessionRoute(hostID: hostID, sessionID: handle.session(), kind: .terminal)

        terminalModels["\(route.hostID)/\(route.sessionID)"] = TerminalSessionModel(handle: handle, events: events,
                                                                                     route: route)

        return route
    }

    /// Attaching takes control of the session from the desktop (§8.4), as
    /// for agents; `closeTerminal` detaches.
    func terminalModel(for route: SessionRoute, session: Session?) -> TerminalSessionModel? {
        let key = "\(route.hostID)/\(route.sessionID)"

        if let model = terminalModels[key] { return model }

        guard let core else { return nil }

        let model = TerminalSessionModel(core: core, route: route, title: session?.title ?? tr("Terminal"))

        terminalModels[key] = model

        return model
    }

    /// Drop the view of a terminal, which detaches from it on the host; the
    /// shell keeps running there.
    func closeTerminal(_ route: SessionRoute) {
        terminalModels["\(route.hostID)/\(route.sessionID)"]?.detach()

        terminalModels["\(route.hostID)/\(route.sessionID)"] = nil
    }

    /// End a session on the host, host tabs included. The host lists its
    /// sessions again once it closed one, which drops the row; a refusal
    /// throws the host's reason.
    func closeSession(_ route: SessionRoute) async throws {
        guard let core else { throw CoreError.Failed(message: tr("The app could not start its core.")) }

        try await core.closeSession(host: route.hostID, session: route.sessionID)
    }

    /// Rename a session on the host, host tabs included. The host lists its
    /// sessions again under the new name, which updates the row; a refusal
    /// throws the host's reason.
    func renameSession(_ route: SessionRoute, title: String) async throws {
        guard let core else { throw CoreError.Failed(message: tr("The app could not start its core.")) }

        try await core.renameSession(host: route.hostID, session: route.sessionID, title: title)
    }

    // MARK: Push notifications

    /// Where hosts send this build's pushes; nil when the build has none.
    static var pushEndpoint: String? {
        let endpoint = Bundle.main.object(forInfoDictionaryKey: "NMTPushEndpoint") as? String

        return endpoint.flatMap { $0.hasPrefix("https://") ? $0 : nil }
    }

    /// Debug builds are signed for the APNs sandbox; archives for TestFlight
    /// and the App Store for production.
    static var pushIsProduction: Bool {
        #if DEBUG
        false
        #else
        true
        #endif
    }

    /// Ask to notify and for an APNs token, once there is a host to hear
    /// from. iOS asks the person only the first time.
    func startPush() async {
        guard Self.pushEndpoint != nil, !hosts.isEmpty else { return }

        do {
            guard try await UNUserNotificationCenter.current()
                .requestAuthorization(options: [.alert, .sound, .badge]) else { return }
        } catch {
            pushLog.error("notification authorization failed: \(error.localizedDescription, privacy: .public)")

            return
        }

        UIApplication.shared.registerForRemoteNotifications()
    }

    func pushTokenReceived(_ token: String) {
        pushToken = token

        for host in hosts where host.isOnline {
            registerPush(host.id)
        }
    }

    /// The notification settings changed: tell every reachable host.
    func setNetworkPreference(_ preference: NetworkPreference) {
        core?.setNetworkMode(mode: preference.mode)
    }

    func pushSettingsChanged() {
        for host in hosts where host.isOnline {
            registerPush(host.id)
        }
    }

    /// Register this phone's pushes with a host, or withdraw them when every
    /// kind is turned off. Hosts too old to take registrations refuse, which
    /// only means that host sends none.
    private func registerPush(_ hostID: String) {
        guard let core, let token = pushToken, let endpoint = Self.pushEndpoint,
              let key = PushKeys.key(for: hostID) else { return }

        let kinds = Self.enabledNotificationKinds()

        Task {
            do {
                if kinds.isEmpty {
                    try await core.unregisterPush(host: hostID)
                } else {
                    try await core.registerPush(host: hostID, settings: PushSettings(
                        endpoint: endpoint, token: token, production: Self.pushIsProduction,
                        key: key, kinds: kinds))
                }

                pushLog.info("push registration sent to a host")
            } catch {
                pushLog.error("push registration failed: \(error.displayText, privacy: .public)")
            }
        }
    }

    /// The kinds the notification switches in Settings leave on.
    private static func enabledNotificationKinds() -> [NotificationKind] {
        let defaults = UserDefaults.standard
        func on(_ key: String) -> Bool { defaults.object(forKey: key) as? Bool ?? true }
        return [
            (on("notifyTurnFinished"), NotificationKind.turnFinished),
            (on("notifyError"), .turnFailed),
            (on("notifyApproval"), .approval),
            (on("notifyQuestion"), .question),
        ].compactMap { $0.0 ? $0.1 : nil }
    }

    /// Open the agent session a tapped notification is about.
    func openFromNotification(host: String, session: String) {
        guard self.host(host) != nil else { return }

        path = NavigationPath()

        path.append(SessionRoute(hostID: host, sessionID: session, kind: .agent))
    }
}
