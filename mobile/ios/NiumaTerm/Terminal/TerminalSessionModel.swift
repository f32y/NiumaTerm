import Observation
import SwiftUI
import UIKit

import NiumaTermCore

/// One terminal session on a host, over `TerminalHandle` (design doc §8.2).
/// The core runs the terminal engine; this carries input to it and hands its
/// frames to the surface that draws them.
@MainActor
@Observable
final class TerminalSessionModel {
    let route: SessionRoute
    var title: String

    private(set) var ended: ViewEnd?
    private(set) var exited = false
    private(set) var cols = 0
    private(set) var rows = 0
    private(set) var background: UInt32 = 0xFFFFFF

    /// Sticky modifiers from the accessory bar; the next key takes them.
    var ctrl = false

    var alt = false

    var notice: String?

    /// The surface has the keyboard.
    var keyboardShown = false

    @ObservationIgnored private(set) var handle: TerminalHandle?
    @ObservationIgnored private let events: TerminalEvents

    @ObservationIgnored weak var surface: TerminalSurface? {
        didSet { surface?.setNeedsFrame() }
    }

    /// Whether the viewport displays scrollback above the live screen.
    private(set) var scrolledBack = false

    /// Attach to a session the host already runs.
    init(core: MobileCore, route: SessionRoute, title: String) {
        self.route = route
        self.title = title
        events = TerminalEvents()
        events.model = self

        let grid = TerminalMetrics.estimatedGrid()

        do {
            handle = try core.attachTerminal(host: route.hostID, session: route.sessionID,
                                             cols: UInt16(grid.cols), rows: UInt16(grid.rows),
                                             observer: events)
        } catch {
            notice = error.displayText
        }
    }

    /// Adopt a session this phone just started.
    init(handle: TerminalHandle, events: TerminalEvents, route: SessionRoute) {
        self.route = route
        title = tr("Terminal")
        self.handle = handle
        self.events = events
        events.model = self
    }

    var attached: Bool { cols > 0 }

    // MARK: Core updates

    func viewChanged() {
        surface?.setNeedsFrame()
    }

    /// The next frame, with the state beside the grid applied here. The
    /// surface calls this once per display refresh while changes are due.
    func nextFrame() -> TerminalFrame? {
        guard let handle else { return nil }

        let frame = handle.frame()

        if cols != Int(frame.cols) { cols = Int(frame.cols) }

        if rows != Int(frame.rows) { rows = Int(frame.rows) }

        if background != frame.background { background = frame.background }

        if !frame.title.isEmpty && frame.title != title { title = frame.title }

        if ended != frame.ended { ended = frame.ended }

        if exited != frame.exited { exited = frame.exited }

        scrolledBack = frame.scrollOffset + UInt64(frame.rows) < frame.scrollTotal

        if frame.bell {
            UIImpactFeedbackGenerator(style: .rigid).impactOccurred()
        }

        // A program on the host sets this phone's clipboard, as it would
        // over SSH.
        if let text = frame.clipboard {
            UIPasteboard.general.string = text
        }

        return frame
    }

    // MARK: Input

    /// Typed or committed text. A single character takes the sticky
    /// modifiers and goes through the key encoder, so Ctrl-C from the bar
    /// becomes the same bytes as from a hardware keyboard.
    func sendText(_ text: String) {
        guard let handle else { return }

        if (ctrl || alt), text.count == 1 {
            _ = sendKey(text.lowercased(), text: text)

            return
        }

        switch text {
        case "\n", "\r":
            _ = sendKey("enter")
        case "\t":
            _ = sendKey("tab")
        default:
            if handle.sendText(text: text) { followInput() }
        }
    }

    @discardableResult
    func sendKey(_ key: String, text: String? = nil, shift: Bool = false, control: Bool = false,
                 alt useAlt: Bool = false, command: Bool = false) -> KeyResult {
        guard let handle else { return .ignored }

        let input = TerminalKeyInput(key: key, text: text, shift: shift,
                                     control: control || ctrl, alt: useAlt || alt, command: command)

        ctrl = false
        alt = false

        let result = handle.sendKey(key: input)

        switch result {
        case .sent:
            followInput()
        case .paste:
            paste()
        case .copy:
            copySelection()
        case .ignored:
            break
        }

        return result
    }

    func paste() {
        guard let handle, let text = UIPasteboard.general.string, !text.isEmpty else { return }

        if handle.paste(text: text) { followInput() }
    }

    func copySelection() {
        guard let handle else { return }

        Task {
            if let text = await handle.selectedText() {
                UIPasteboard.general.string = text

                handle.clearSelection()
            }
        }
    }

    func press(_ key: AccessoryKey) {
        switch key.action {
        case .ctrl: ctrl.toggle()
        case .alt: alt.toggle()
        case .esc: sendKey("escape")
        case .tab: sendKey("tab")
        case .arrow(let name), .nav(let name): sendKey(name)
        case .text(let text): sendText(text)
        case .paste: paste()
        }

        if key.action != .ctrl && key.action != .alt {
            UIImpactFeedbackGenerator(style: .light).impactOccurred()
        }
    }

    /// Typing while reading history returns to the live screen, where the
    /// typed text shows up.
    private func followInput() {
        if scrolledBack {
            _ = scrollToBottom()
        }
    }

    func scrollToBottom() -> Bool {
        handle?.scrollToBottom() ?? false
    }

    func toggleKeyboard() {
        guard let surface else { return }

        _ = keyboardShown ? surface.resignFirstResponder() : surface.becomeFirstResponder()
    }

    // MARK: Session

    func resize(cols: Int, rows: Int, width: Int, height: Int) {
        _ = handle?.resize(cols: UInt16(clamping: cols), rows: UInt16(clamping: rows),
                           width: UInt16(clamping: width), height: UInt16(clamping: height))
    }

    /// Take the session back after the desktop took it (§8.4).
    func takeControl() {
        handle?.takeControl()
    }

    /// End the shell on the host. Host tabs refuse; they belong to the
    /// person at the computer.
    func terminate() {
        handle?.terminate()
    }

    /// Drop the view, which detaches from the session on the host.
    func detach() {
        handle = nil
    }
}

struct AccessoryKey: Identifiable {
    enum Action: Equatable {
        case esc, tab, ctrl, alt, paste
        case arrow(String)
        case nav(String)
        case text(String)
    }

    let label: String
    let action: Action
    var id: String { label }

    /// Default layout (§8.2). Arrow and navigation names are the ones the
    /// core's key encoder takes.
    static let defaultLayout: [AccessoryKey] = [
        .init(label: "Esc", action: .esc),
        .init(label: "Tab", action: .tab),
        .init(label: "Ctrl", action: .ctrl),
        .init(label: "Alt", action: .alt),
        .init(label: "←", action: .arrow("left")),
        .init(label: "↑", action: .arrow("up")),
        .init(label: "↓", action: .arrow("down")),
        .init(label: "→", action: .arrow("right")),
        .init(label: "|", action: .text("|")),
        .init(label: "~", action: .text("~")),
        .init(label: "/", action: .text("/")),
        .init(label: "-", action: .text("-")),
        .init(label: "Home", action: .nav("home")),
        .init(label: "End", action: .nav("end")),
        .init(label: "PgUp", action: .nav("pageup")),
        .init(label: "PgDn", action: .nav("pagedown")),
        .init(label: "Paste", action: .paste),
    ]
}
