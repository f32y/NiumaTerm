import SwiftUI

/// Settings (§8.1): paired hosts, notifications, terminal, agent, security, appearance, about.
struct SettingsView: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss

    @AppStorage("notifyTurnFinished") private var notifyTurnFinished = true
    @AppStorage("notifyApproval") private var notifyApproval = true
    @AppStorage("notifyQuestion") private var notifyQuestion = true
    @AppStorage("notifyError") private var notifyError = true
    @AppStorage("terminalFontName") private var fontName = "JetBrains Mono"
    @AppStorage("terminalFontSize") private var fontSize = 11.0
    @AppStorage("transcriptMono") private var transcriptMono = true
    @AppStorage("faceIDLock") private var faceIDLock = false
    @AppStorage("appearance") private var appearance = AppAppearance.system
    @AppStorage(ComposerSettings.codexSkillsInSlashKey) private var codexSkillsInSlash = true
    @AppStorage(ComposerSettings.questionsOneAtATimeKey) private var questionsOneAtATime = false
    @AppStorage(NetworkPreference.storageKey) private var networkPreference = NetworkPreference.auto

    @Bindable private var localization = Localization.shared
    @State private var forgetting: Host?

    /// Pair another computer; the list presenting Settings shows the pairing
    /// screen once Settings is gone.
    let onAddTarget: () -> Void

    var body: some View {
        NavigationStack {
            Form {
                Section("Paired computers") {
                    ForEach(app.hosts) { host in
                        HStack(spacing: 12) {
                            Image(systemName: host.icon).foregroundStyle(Theme.ink2).frame(width: 24)

                            VStack(alignment: .leading, spacing: 2) {
                                Text(host.name)

                                Text(host.statusText).font(.caption).foregroundStyle(Theme.secondary)
                            }

                            Spacer()

                            Button("Forget", role: .destructive) { forgetting = host }
                                .buttonStyle(.borderless)
                        }
                    }

                    Button(action: onAddTarget) {
                        Label("Add target", systemImage: "qrcode.viewfinder")
                    }
                }

                Section {
                    Toggle("Turn finished", isOn: $notifyTurnFinished)
                    Toggle("Approval needed", isOn: $notifyApproval)
                    Toggle("Questions", isOn: $notifyQuestion)
                    Toggle("Errors", isOn: $notifyError)
                } header: {
                    Text("Notifications")
                } footer: {
                    Text("A computer sends these once this phone has been away from it for five minutes, and stops when you are back at the computer.")
                }

                Section("Terminal") {
                    Picker("Font", selection: $fontName) {
                        Text("JetBrains Mono").tag("JetBrains Mono")
                        Text("SF Mono").tag("SF Mono")
                    }

                    Stepper(value: $fontSize, in: 8...18, step: 1) {
                        LabeledContent("Size", value: "\(Int(fontSize)) pt")
                    }
                }

                Section {
                    Toggle("Monospaced transcript", isOn: $transcriptMono)
                    Toggle("Codex skills in / list", isOn: $codexSkillsInSlash)
                    Toggle("Answer questions one at a time", isOn: $questionsOneAtATime)
                } header: {
                    Text("Agent")
                } footer: {
                    Text("List Codex skills among the / commands; picking one writes its $name form. With this off, / lists commands only and $ lists skills. When an agent asks several questions together, answering one at a time shows them one by one.")
                }

                Section("Security") {
                    Toggle("Require Face ID", isOn: $faceIDLock)
                }

                Section("Appearance") {
                    Picker("Theme", selection: $appearance) {
                        ForEach(AppAppearance.allCases) { option in
                            Text(option.label).tag(option)
                        }
                    }
                }

                Section {
                    Picker("Connection", selection: $networkPreference) {
                        ForEach(NetworkPreference.allCases) { option in
                            Text(option.label).tag(option)
                        }
                    }
                } header: {
                    Text("Network")
                } footer: {
                    Text("Automatic uses the local network when it reaches the computer, and the relay otherwise. Always Relay or Always LAN keeps every connection on that path; a computer paired without a relay cannot be reached with Always Relay.")
                }

                Section {
                    Picker("Language", selection: $localization.language) {
                        ForEach(AppLanguage.allCases) { option in
                            Text(option.label).tag(option)
                        }
                    }
                }

                Section {
                    LabeledContent("Version", value: AppModel.appVersion)
                }

                Section {
                    NavigationLink {
                        LogsView()
                    } label: {
                        Label("Show logs", systemImage: "doc.text.magnifyingglass")
                    }
                }
            }
            .tint(Theme.accent)
            .onChange(of: [notifyTurnFinished, notifyApproval, notifyQuestion, notifyError]) {
                app.pushSettingsChanged()
            }
            .onChange(of: networkPreference) {
                app.setNetworkPreference(networkPreference)
            }
            .navigationTitle(tr("Settings"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
            }
            .confirmationDialog(
                "Forget \(forgetting?.name ?? "")?",
                isPresented: Binding(get: { forgetting != nil }, set: { if !$0 { forgetting = nil } }),
                titleVisibility: .visible,
                presenting: forgetting
            ) { host in
                Button("Forget \(host.name)", role: .destructive) { app.forget(host.id) }
            } message: { _ in
                Text("This phone will need to scan a new QR code to reach it again.")
            }
        }
    }
}
