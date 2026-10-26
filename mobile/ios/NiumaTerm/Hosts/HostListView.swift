import SwiftUI

import NiumaTermCore

/// Root: paired hosts, each a section listing its sessions (design doc §8.1).
struct HostListView: View {
    @Environment(AppModel.self) private var app
    @State private var newSessionHost: Host?
    @State private var pairing: PairingRequest?
    @State private var showSettings = false
    @State private var pairAfterSettings = false
    @State private var closing: SessionClose?
    @State private var renaming: SessionRoute?
    @State private var renameTitle = ""
    @State private var failure: ActionFailure?
    @State private var starting = false

    /// Folds by host, and by host and workspace, for this run of the app.
    @State private var folds: [String: SessionFold] = [:]

    var body: some View {
        @Bindable var app = app

        NavigationStack(path: $app.path) {
            List {
                if let error = app.startupError {
                    Section {
                        Text(error).foregroundStyle(Theme.attention)
                    } header: {
                        Text("NiumaTerm could not start")
                    }
                }

                ForEach(app.hosts) { host in
                    let groups = host.sessionGroups
                    let hostFold = folds[host.id] ?? .all

                    let header = HostHeader(host: host,
                                            fold: groups.isEmpty ? nil : hostFold,
                                            onFold: { fold(host.id, over: host.sessions) },
                                            onNew: host.isOnline ? { newSessionHost = host } : nil,
                                            onRetry: host.status == .unreachable ? { app.retry(host.id) } : nil)
                        .textCase(nil)

                    if groups.isEmpty || hostFold == .collapsed {
                        Section {
                            if groups.isEmpty && host.isOnline {
                                Text("No sessions. Tap + to start a terminal or an agent.")
                                    .font(.system(size: 14))
                                    .foregroundStyle(Theme.tertiary)
                                    .listRowBackground(Theme.rowBackground)
                            }
                        } header: {
                            header
                        }
                    } else {
                        // One section per workspace; the first also carries
                        // the host's header, so each host still reads as one
                        // block.
                        ForEach(groups) { group in
                            let key = "\(host.id)/\(group.id)"
                            let groupFold: SessionFold = group.workspace == nil ? .all : folds[key] ?? .all

                            Section {
                                ForEach(group.sessions.filter { hostFold.shows($0) && groupFold.shows($0) }) { session in
                                    sessionRow(session, on: host)
                                }
                            } header: {
                                VStack(alignment: .leading, spacing: 10) {
                                    if group.id == groups.first?.id {
                                        header
                                    }

                                    if let workspace = group.workspace {
                                        WorkspaceHeader(workspace: workspace,
                                                        fold: groupFold,
                                                        agents: host.offer?.agents ?? [],
                                                        enabled: host.isOnline && !starting,
                                                        onFold: { fold(key, over: group.sessions) },
                                                        onTerminal: { openTerminal(on: host) },
                                                        onAgent: { openAgent($0, in: workspace, on: host) })
                                    }
                                }
                            }
                        }
                    }
                }
            }
            .listStyle(.insetGrouped)
            .scrollContentBackground(.hidden)
            .background(Theme.canvas)
            .overlay {
                if app.hosts.isEmpty && app.startupError == nil {
                    ContentUnavailableView {
                        Label("No computers yet", systemImage: "desktopcomputer")
                    } description: {
                        Text("Pair this phone with NiumaTerm on your computer to use its terminals and agents here.")
                    } actions: {
                        Button("Add computer") { pairing = PairingRequest(link: nil) }
                            .buttonStyle(.borderedProminent)
                    }
                }
            }
            .navigationTitle(tr("Computers"))
            .navigationDestination(for: SessionRoute.self) { SessionScreen(route: $0) }
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button("Settings", systemImage: "gearshape") { showSettings = true }
                }
            }
            .alert(
                "Close “\(closing?.title ?? "")”?",
                isPresented: Binding(get: { closing != nil }, set: { if !$0 { closing = nil } }),
                presenting: closing
            ) { request in
                Button("Cancel", role: .cancel) {}
                Button("Close Session", role: .destructive) { close(request) }
            } message: { request in
                Text("The session ends on \(request.hostName), along with anything running in it.")
            }
            .alert(
                "Rename session",
                isPresented: Binding(get: { renaming != nil }, set: { if !$0 { renaming = nil } }),
                presenting: renaming
            ) { route in
                TextField("Title", text: $renameTitle)
                Button("Cancel", role: .cancel) {}

                Button("Save") { rename(route) }
                    .disabled(renameTitle.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
            .alert(
                failure?.title ?? "",
                isPresented: Binding(get: { failure != nil }, set: { if !$0 { failure = nil } }),
                presenting: failure
            ) { _ in
                Button("OK", role: .cancel) {}
            } message: { failure in
                Text(failure.message)
            }
            .sheet(item: $newSessionHost) { host in
                NewSessionSheet(host: host)
            }
            // Pairing opens only once Settings has gone, so the two screens
            // never try to present at the same time.
            .sheet(isPresented: $showSettings, onDismiss: {
                if pairAfterSettings {
                    pairAfterSettings = false
                    pairing = PairingRequest(link: nil)
                }
            }) {
                SettingsView(onAddTarget: {
                    pairAfterSettings = true
                    showSettings = false
                })
            }
            .fullScreenCover(item: $pairing) { request in
                PairingView(initialLink: request.link)
            }
            // The Camera app hands a scanned niumaterm://pair link to the
            // app, which pairs as if it had scanned the code itself.
            .onOpenURL { url in
                guard url.scheme == "niumaterm", url.host() == "pair" else { return }

                pairing = PairingRequest(link: url.absoluteString)
            }
        }
    }

    private func sessionRow(_ session: Session, on host: Host) -> some View {
        let route = SessionRoute(hostID: host.id, sessionID: session.id, kind: session.kind)

        return NavigationLink(value: route) {
            SessionRow(session: session)
        }
        .listRowBackground(Theme.rowBackground)
        // No destructive role: SwiftUI would slide the row out at once,
        // before the person confirms or the host agrees to close the session.
        .swipeActions(edge: .trailing, allowsFullSwipe: false) {
            if host.isOnline {
                Button("Close", systemImage: "xmark") {
                    closing = SessionClose(route: route, title: session.title, hostName: host.name)
                }
                .tint(.red)

                Button("Rename", systemImage: "pencil") {
                    renameTitle = session.title
                    renaming = route
                }
                .tint(Theme.accent)
            }
        }
    }

    /// Step the fold at `key` on a tap. The change animates, so the rows
    /// slide rather than vanish.
    private func fold(_ key: String, over sessions: [Session]) {
        let next = (folds[key] ?? .all).next(anyAsleep: sessions.contains { $0.pending })

        withAnimation(.snappy) { folds[key] = next }
    }

    private func close(_ request: SessionClose) {
        Task {
            do {
                try await app.closeSession(request.route)
            } catch {
                failure = ActionFailure(title: tr("Could not close the session"), message: error.displayText)
            }
        }
    }

    private func rename(_ route: SessionRoute) {
        let title = renameTitle.trimmingCharacters(in: .whitespacesAndNewlines)

        Task {
            do {
                try await app.renameSession(route, title: title)
            } catch {
                failure = ActionFailure(title: tr("Could not rename the session"), message: error.displayText)
            }
        }
    }

    /// The host has no way to start a shell in a given directory, so a
    /// terminal from a workspace row starts where the host's shells do.
    private func openTerminal(on host: Host) {
        start { try await app.openTerminal(hostID: host.id) }
    }

    private func openAgent(_ profile: AgentProfileRecord, in workspace: WorkspaceRecord, on host: Host) {
        start { try await app.openAgent(hostID: host.id, profile: profile.name, workspace: workspace.path) }
    }

    /// Start one session at a time, and show its screen once the host
    /// answers with it.
    private func start(_ open: @escaping () async throws -> SessionRoute) {
        starting = true

        Task {
            defer { starting = false }

            do {
                app.path.append(try await open())
            } catch {
                failure = ActionFailure(title: tr("Could not start the session"), message: error.displayText)
            }
        }
    }
}

/// A request that failed, held while its alert shows.
struct ActionFailure {
    let title: String
    let message: String
}

/// A session the person asked to close, held while they confirm.
struct SessionClose {
    let route: SessionRoute
    let title: String
    let hostName: String
}

/// One presentation of the pairing screen. The link travels with it, so
/// the screen sees the link it was opened for rather than a value captured
/// when the cover was first declared.
struct PairingRequest: Identifiable {
    let id = UUID()
    let link: String?
}

struct HostHeader: View {
    let host: Host

    /// Nil while the host lists nothing to fold.
    var fold: SessionFold?

    var onFold: () -> Void
    var onNew: (() -> Void)?
    var onRetry: (() -> Void)?

    var body: some View {
        HStack(spacing: 10) {
            if let fold {
                FoldMark(fold: fold)
            }

            Image(systemName: host.icon)
                .font(.system(size: 18))
                .foregroundStyle(Theme.ink)

            VStack(alignment: .leading, spacing: 2) {
                Text(host.name)
                    .font(.system(size: 17, weight: .semibold))
                    .foregroundStyle(Theme.ink)

                HStack(spacing: 5) {
                    Circle().fill(host.statusColor).frame(width: 7, height: 7)

                    Text(host.statusText)
                        .font(.system(size: 12))
                        .foregroundStyle(Theme.secondary)
                }
            }

            Spacer()

            if let onRetry {
                Button("Retry", action: onRetry)
                    .font(.system(size: 14, weight: .semibold))
                    .foregroundStyle(Theme.accent)
                    .buttonStyle(.plain)
                    .accessibilityLabel("Retry connecting to \(host.name)")
            }

            if let onNew {
                Button(action: onNew) {
                    Image(systemName: "plus")
                        .font(.system(size: 14, weight: .bold))
                        .foregroundStyle(Theme.accent)
                        .frame(width: 32, height: 32)
                        .background(Theme.accent.opacity(0.1), in: .circle)
                }
                .buttonStyle(.plain)
                .accessibilityLabel("New session on \(host.name)")
            }
        }
        .padding(.top, 8)
        .opacity(host.isOnline ? 1 : 0.7)
        // The buttons inside keep their own taps; the rest of the header
        // folds the host.
        .contentShape(.rect)
        .onTapGesture { if fold != nil { onFold() } }
        .accessibilityAction(named: Text("Fold")) { if fold != nil { onFold() } }
    }
}

/// The disclosure triangle of a foldable header, which names the awake
/// fold too, since that fold looks like the full list at a glance.
struct FoldMark: View {
    let fold: SessionFold

    var body: some View {
        HStack(spacing: 4) {
            Image(systemName: "chevron.right")
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(Theme.tertiary)
                .rotationEffect(.degrees(fold == .collapsed ? 0 : 90))

            if fold == .awake {
                Image(systemName: "moon.zzz")
                    .font(.system(size: 10))
                    .foregroundStyle(Theme.tertiary)
                    .accessibilityLabel("Sleeping sessions hidden")
            }
        }
        .frame(minWidth: 12, alignment: .leading)
    }
}

/// A workspace the host offers: its name and path, a tap that folds its
/// sessions, and a menu that starts a terminal or one of the host's agents.
struct WorkspaceHeader: View {
    let workspace: WorkspaceRecord
    let fold: SessionFold
    let agents: [AgentProfileRecord]
    let enabled: Bool
    var onFold: () -> Void
    var onTerminal: () -> Void
    var onAgent: (AgentProfileRecord) -> Void

    var body: some View {
        HStack(spacing: 6) {
            FoldMark(fold: fold)

            Image(systemName: "folder")
                .font(.system(size: 12))
                .foregroundStyle(Theme.secondary)

            Text(workspace.name)
                .font(.system(size: 12.5, weight: .medium))
                .foregroundStyle(Theme.secondary)
                .lineLimit(1)
                .layoutPriority(1)

            Text(workspace.path)
                .font(Theme.mono(10.5))
                .foregroundStyle(Theme.tertiary)
                .lineLimit(1)
                .truncationMode(.head)

            Spacer(minLength: 4)

            Menu {
                Button(action: onTerminal) {
                    Text(verbatim: ">_  ") + Text("New terminal")
                }

                if !agents.isEmpty {
                    Divider()
                }

                ForEach(agents, id: \.name) { agent in
                    Button { onAgent(agent) } label: {
                        Text(verbatim: "\(AgentProfile(harness: agent.harness).glyph)  \(agent.name)")
                    }
                }
            } label: {
                Image(systemName: "plus")
                    .font(.system(size: 12, weight: .bold))
                    .foregroundStyle(Theme.accent)
                    .frame(width: 28, height: 24)
                    .contentShape(.rect)
            }
            .disabled(!enabled)
            .accessibilityLabel("New session in \(workspace.name)")
        }
        .textCase(nil)
        .contentShape(.rect)
        .onTapGesture(perform: onFold)
        .accessibilityAction(named: Text("Fold"), onFold)
    }
}

struct SessionRow: View {
    let session: Session

    var body: some View {
        HStack(spacing: 12) {
            Group {
                if session.pending {
                    Image(systemName: "moon")
                        .font(.system(size: 13, weight: .medium))
                } else {
                    Text(session.glyph)
                        .font(Theme.mono(13, weight: .medium))
                }
            }
            .foregroundStyle(Theme.ink2)
            .frame(width: 22)

            VStack(alignment: .leading, spacing: 2) {
                Text(session.title)
                    .font(.system(size: 16))
                    .foregroundStyle(Theme.ink)
                    .lineLimit(1)

                Text(session.subtitle)
                    .font(.system(size: 12.5))
                    .foregroundStyle(Theme.secondary)
            }

            Spacer(minLength: 4)
        }
        .padding(.vertical, 3)
        // A tab still asleep on the host fades, as the desktop draws one.
        .opacity(session.pending ? 0.55 : 1)
    }
}

/// Routes a session to its terminal or agent screen.
struct SessionScreen: View {
    @Environment(AppModel.self) private var app
    let route: SessionRoute

    var body: some View {
        let session = app.session(route)
        let hostName = app.host(route.hostID)?.name ?? tr("Computer")

        switch route.kind {
        case .agent:
            if let model = app.agentModel(for: route, session: session) {
                AgentSessionView(model: model, hostName: hostName)
                    .onDisappear { app.closeAgent(route) }
            }
        case .terminal:
            if let model = app.terminalModel(for: route, session: session) {
                TerminalSessionView(model: model, hostName: hostName)
                    .onDisappear { app.closeTerminal(route) }
            }
        case .other:
            ContentUnavailableView("Session ended", systemImage: "xmark.circle",
                                   description: Text("\(hostName) no longer lists this session."))
        }
    }
}
