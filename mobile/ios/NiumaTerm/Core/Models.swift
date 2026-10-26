import SwiftUI

import NiumaTermCore

/// An agent as a host names it in `host.info` and on its sessions.
struct AgentProfile: Hashable {
    let harness: String

    var displayName: String {
        switch harness {
        case "claude": "Claude Code"
        case "codex": "Codex"
        case "deepseek": "DeepSeek"
        default: harness.capitalized
        }
    }

    var shortName: String {
        switch harness {
        case "claude": "Claude"
        case "codex": "Codex"
        case "deepseek": "DeepSeek"
        default: harness.capitalized
        }
    }

    var glyph: String {
        switch harness {
        case "claude": "✱"
        case "codex": "◎"
        case "deepseek": "◆"
        default: "◇"
        }
    }
}

struct Session: Identifiable, Hashable {
    let id: String
    let hostID: String
    var title: String
    var kind: SessionType
    var profile: AgentProfile?
    var workspace: SessionWorkspace?

    /// A host tab still asleep; opening it starts its shell or agent.
    var pending: Bool

    init(record: SessionRecord, hostID: String) {
        id = record.id
        self.hostID = hostID
        title = record.title
        kind = record.kind
        profile = record.harness.map(AgentProfile.init(harness:))

        workspace = record.workspace.map {
            SessionWorkspace(id: $0.id, name: $0.name, position: Int($0.position))
        }

        pending = record.pending
    }

    var glyph: String {
        switch kind {
        case .terminal: ">_"
        case .agent: profile?.glyph ?? "✱"
        case .other: "?"
        }
    }

    var subtitle: String {
        let kindName = switch kind {
        case .terminal: tr("Terminal")
        case .agent: profile?.displayName ?? tr("Agent")
        case .other: tr("Session")
        }

        return pending ? tr("\(kindName) · Asleep") : kindName
    }
}

/// The host workspace whose tab shows a session.
struct SessionWorkspace: Hashable {
    let id: String
    let name: String
    let position: Int
}

/// A workspace the host offers with the sessions its tabs hold, listed
/// even while it holds none, since new sessions start from it. `workspace`
/// is nil for the sessions no offered workspace holds.
struct SessionGroup: Identifiable {
    let id: String
    let workspace: WorkspaceRecord?
    let sessions: [Session]
}

/// How much of a host or workspace the list shows. Folds last for the app
/// run only: a host's workspace ids are runtime ids, which name other
/// workspaces once the host restarts.
enum SessionFold {
    case all
    case collapsed
    /// Only the sessions that are running. A restored host starts just the
    /// tab in front and keeps the rest asleep, so the sleeping ones are what
    /// makes a long list long.
    case awake

    /// The fold a tap moves to: every session, none, then the awake ones.
    /// The awake step is skipped while nothing is asleep, where it would
    /// list the same rows as `all`.
    func next(anyAsleep: Bool) -> SessionFold {
        switch self {
        case .all: .collapsed
        case .collapsed: anyAsleep ? .awake : .all
        case .awake: .all
        }
    }

    func shows(_ session: Session) -> Bool {
        switch self {
        case .all: true
        case .collapsed: false
        case .awake: !session.pending
        }
    }
}

/// A route on the navigation stack: one session on one host. It carries
/// the kind because a session just opened here may reach the host's list
/// only after its screen is already showing.
struct SessionRoute: Hashable {
    let hostID: String
    let sessionID: String
    let kind: SessionType
}

/// Which network links to computers may use. Stored by raw value in user
/// defaults, so the cases' names must not change.
enum NetworkPreference: String, CaseIterable, Identifiable {
    case auto
    case relay
    case lan

    static let storageKey = "networkMode"

    var id: String { rawValue }

    var label: String {
        switch self {
        case .auto: tr("Automatic")
        case .relay: tr("Always Relay")
        case .lan: tr("Always LAN")
        }
    }

    var mode: NetworkMode {
        switch self {
        case .auto: .auto
        case .relay: .relay
        case .lan: .lan
        }
    }

    static var stored: NetworkPreference {
        UserDefaults.standard.string(forKey: storageKey).flatMap(NetworkPreference.init(rawValue:)) ?? .auto
    }
}

struct Host: Identifiable {
    let id: String
    var name: String
    var status: HostStatus

    /// How the host is reached, while it is connected.
    var link: HostLink?

    var sessions: [Session] = []
    var sessionsLoaded = false
    var sessionLoadError: String?

    /// The agents and workspaces the host offers, from the same moment as
    /// `sessions`; nil when the host did not say.
    var offer: HostOffer?

    init(record: HostRecord) {
        id = record.id
        name = record.name
        status = record.status
        link = record.link
    }

    var isOnline: Bool { status == .connected }

    var isLoadingSessions: Bool {
        status == .connecting || status == .reconnecting
            || (isOnline && !sessionsLoaded && sessionLoadError == nil)
    }

    var icon: String { "desktopcomputer" }

    /// The network the link runs over, as session title bars show it.
    var linkText: String? {
        switch link {
        case .lan: tr("LAN")
        case .relay: tr("Relay")
        case .direct: tr("Direct")
        case nil: nil
        }
    }

    var statusText: String {
        switch status {
        case .connected: tr("Online")
        case .connecting: tr("Connecting…")
        case .reconnecting: tr("Reconnecting…")
        case .idle: tr("Not connected")
        case .refused: tr("No longer trusts this phone · pair again")
        case .unreachable: tr("Connection failed")
        }
    }

    var statusColor: Color {
        switch status {
        case .connected: Theme.online
        case .connecting, .reconnecting: Theme.attention
        case .idle: Theme.offline
        case .refused, .unreachable: Theme.attention
        }
    }

    /// The sessions no offered workspace holds first, right under the host,
    /// since after the last workspace they would read as its sessions; then
    /// every offered workspace in the host's order. Within a group terminals
    /// come first, then agents, as the design lists them.
    var sessionGroups: [SessionGroup] {
        let workspaces = offer?.workspaces ?? []
        let ordered = sessions.filter { $0.kind == .terminal } + sessions.filter { $0.kind != .terminal }

        // Hosts that send workspace ids are matched by them, so workspaces
        // sharing a name keep their own sessions; older hosts send none,
        // and their sessions fall back to the workspace's name.
        func holds(_ workspace: WorkspaceRecord, _ session: Session) -> Bool {
            guard let held = session.workspace else { return false }

            if let id = workspace.id { return held.id == id }

            return held.name == workspace.name
        }

        let loose = ordered.filter { session in !workspaces.contains { holds($0, session) } }

        var groups = loose.isEmpty ? [] : [SessionGroup(id: "", workspace: nil, sessions: loose)]

        for (index, workspace) in workspaces.enumerated() {
            groups.append(SessionGroup(id: workspace.id ?? "\(index):\(workspace.name)",
                                       workspace: workspace,
                                       sessions: ordered.filter { holds(workspace, $0) }))
        }

        return groups
    }
}

extension Error {
    /// The core words its errors for people already.
    var displayText: String {
        if let core = self as? CoreError, case .Failed(let message) = core {
            return message
        }

        return localizedDescription
    }
}
