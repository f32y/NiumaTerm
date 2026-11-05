import SwiftUI

import NiumaTermCore

struct TerminalSessionView: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss

    /// The app's appearance, from the system or the theme setting. The
    /// screen below overrides the scheme to match the terminal; sheets over
    /// it follow the app instead.
    @Environment(\.colorScheme) private var appScheme

    @Bindable var model: TerminalSessionModel
    let hostName: String

    @AppStorage("terminalFontSize") private var fontSize = 11.0
    @AppStorage("terminalFontName") private var fontName = "JetBrains Mono"

    private var hostStatus: HostStatus? { app.host(model.route.hostID)?.status }
    private var linkText: String? { app.host(model.route.hostID)?.linkText }

    /// The screen's chrome follows the terminal's background, which the
    /// program's theme decides, so bars and titles stay readable on it.
    private var scheme: ColorScheme {
        let rgb = model.background
        let luma = 0.299 * Double((rgb >> 16) & 0xFF) + 0.587 * Double((rgb >> 8) & 0xFF) + 0.114 * Double(rgb & 0xFF)

        return luma < 128 ? .dark : .light
    }

    var body: some View {
        TerminalSurfaceView(model: model, fontSize: fontSize, fontName: fontName) { fontSize = $0 }
            .background(Color(hex: model.background))
            .overlay {
                if !model.attached {
                    VStack(spacing: 10) {
                        if let notice = model.notice {
                            Text(notice).foregroundStyle(Theme.attention)
                        } else if hostStatus == .unreachable {
                            Text("Could not connect to \(hostName).").foregroundStyle(Theme.attention)

                            Button("Retry") { app.retry(model.route.hostID) }
                                .buttonStyle(.bordered)
                        } else {
                            ProgressView()
                            Text("Connecting to \(hostName)…")
                        }
                    }
                    .font(.system(size: 15))
                    .foregroundStyle(.secondary)
                    .padding(40)
                }
            }
            .overlay(alignment: .top) {
                if model.attached && hostStatus == .unreachable {
                    Button { app.retry(model.route.hostID) } label: {
                        Label("Cannot reach \(hostName) · Retry", systemImage: "wifi.exclamationmark")
                            .font(.system(size: 13, weight: .medium))
                            .foregroundStyle(Theme.attention)
                            .padding(.horizontal, 14)
                            .frame(height: 34)
                            .glassCapsule()
                    }
                    .buttonStyle(.plain)
                    .padding(.top, 8)
                } else if model.attached && (hostStatus == .reconnecting || hostStatus == .connecting) {
                    Label("Reconnecting to \(hostName)…", systemImage: "wifi.exclamationmark")
                        .font(.system(size: 13, weight: .medium))
                        .foregroundStyle(.primary)
                        .padding(.horizontal, 14)
                        .frame(height: 34)
                        .glassCapsule()
                        .padding(.top, 8)
                }
            }
            .overlay(alignment: .bottom) {
                if model.attached && model.scrolledBack {
                    ScrollToBottomButton {
                        if model.scrollToBottom() { model.surface?.setNeedsFrame() }
                    }
                }
            }
            .safeAreaInset(edge: .bottom) {
                if model.exited && model.ended == nil {
                    ExitedBar { dismiss() }
                } else {
                    AccessoryBar(model: model)
                }
            }
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .principal) {
                    VStack(spacing: 1) {
                        Text(model.title).font(.system(size: 16, weight: .semibold)).lineLimit(1)

                        Text([hostName, linkText, model.attached ? "\(model.cols)×\(model.rows)" : nil]
                                .compactMap { $0 }.joined(separator: " · "))
                            .font(Theme.mono(11.5))
                            .foregroundStyle(.secondary)
                    }
                }

                ToolbarItem(placement: .topBarTrailing) {
                    Menu {
                        Button("Paste", systemImage: "doc.on.clipboard") { model.paste() }

                        Button("End session", systemImage: "xmark.circle", role: .destructive) {
                            model.terminate()
                        }
                    } label: {
                        Image(systemName: "ellipsis")
                    }
                    .disabled(!model.attached || model.exited)
                }
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
                    // The sheet belongs to the app, not the terminal: it
                    // takes the app's appearance and a solid background
                    // rather than glass tinted by the terminal behind it.
                    .environment(\.colorScheme, appScheme)
                    .presentationBackground(Theme.sheet)
            }
            .toolbarColorScheme(scheme, for: .navigationBar)
            .environment(\.colorScheme, scheme)
    }
}

/// The shell ended on the host.
private struct ExitedBar: View {
    var onClose: () -> Void

    var body: some View {
        HStack {
            Text("The shell exited.")
                .font(.system(size: 15))
                .foregroundStyle(.secondary)

            Spacer()

            Button("Close", action: onClose)
                .font(.system(size: 15, weight: .semibold))
                .padding(.horizontal, 18)
                .frame(height: 40)
                .glassCapsule(interactive: true)
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 8)
    }
}

/// Glass key row above the keyboard (§8.2).
struct AccessoryBar: View {
    @Bindable var model: TerminalSessionModel

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 6) {
                ForEach(AccessoryKey.defaultLayout) { key in
                    let on = (key.action == .ctrl && model.ctrl) || (key.action == .alt && model.alt)

                    Button { model.press(key) } label: {
                        // The other keys are named as printed on keyboards,
                        // which stay the same in every language.
                        Text(key.action == .paste ? tr("Paste") : key.label)
                            .font(Theme.mono(14, weight: .medium))
                            .foregroundStyle(on ? Theme.onAccent : Color.primary)
                            .padding(.horizontal, 10)
                            .frame(minWidth: 40, minHeight: 40)
                            .background(on ? Theme.accent : Color.clear, in: .capsule)
                            .glassCapsule(interactive: true)
                    }
                    .buttonStyle(.plain)
                }

                Button { model.toggleKeyboard() } label: {
                    Image(systemName: model.keyboardShown ? "keyboard.chevron.compact.down" : "keyboard")
                        .font(.system(size: 15))
                        .foregroundStyle(.primary)
                        .frame(width: 44, height: 40)
                        .glassCapsule(interactive: true)
                }
                .buttonStyle(.plain)
                .accessibilityLabel(model.keyboardShown ? "Hide keyboard" : "Show keyboard")
            }
            .padding(.horizontal, 8)
        }
        .padding(.vertical, 6)
        .disabled(!model.attached)
    }
}
