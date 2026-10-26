import SwiftUI

import NiumaTermCore

/// Floating glass composer: queue, text field, pickers from the `settings` slot, Send/Interrupt.
struct ComposerView: View {
    @Bindable var model: AgentSessionModel
    var focused: FocusState<Bool>.Binding
    @AppStorage(ComposerSettings.codexSkillsInSlashKey) private var codexSkillsInSlash = true

    var body: some View {
        VStack(spacing: 8) {
            ForEach(Array(model.queue.enumerated()), id: \.offset) { _, message in
                HStack(spacing: 8) {
                    Text("QUEUED")
                        .font(Theme.mono(10.5, weight: .semibold))
                        .foregroundStyle(Theme.accent)

                    Text(message.text)
                        .font(.system(size: 13))
                        .foregroundStyle(Theme.ink2)
                        .lineLimit(1)

                    Spacer()

                    if message.id != nil {
                        Button { model.withdraw(message) } label: {
                            Image(systemName: "xmark")
                                .font(.system(size: 10, weight: .bold))
                                .foregroundStyle(Theme.ink2)
                                .frame(width: 26, height: 26)
                                .background(Theme.fill, in: .circle)
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel("Withdraw")
                    }
                }
                .padding(.leading, 14)
                .padding(.trailing, 8)
                .padding(.vertical, 6)
                .glassRounded(16)
                .transition(.move(edge: .bottom).combined(with: .opacity))
            }

            let suggestions = model.suggestions(skillsInSlash: codexSkillsInSlash)

            if !suggestions.isEmpty {
                ScrollView {
                    VStack(alignment: .leading, spacing: 0) {
                        ForEach(suggestions) { suggestion in
                            Button { model.applySuggestion(suggestion) } label: {
                                SuggestionRow(suggestion: suggestion)
                            }
                            .buttonStyle(.plain)
                            .disabled(!suggestion.enabled)
                        }
                    }
                    .padding(.vertical, 6)
                }
                .scrollBounceBehavior(.basedOnSize)
                .frame(maxHeight: 240)
                .fixedSize(horizontal: false, vertical: true)
                .glassRounded(20)
                .transition(.move(edge: .bottom).combined(with: .opacity))
            }

            VStack(alignment: .leading, spacing: 10) {
                TextField("Message \(model.agentName)", text: $model.draft, axis: .vertical)
                    .lineLimit(1...6)
                    .font(.system(size: 15))
                    .focused(focused)
                    .disabled(model.ended != nil)

                HStack(spacing: 6) {
                    // Starts a command only from an empty draft, so a tap
                    // never replaces what is already typed.
                    Button {
                        model.draft = "/"
                        focused.wrappedValue = true
                    } label: {
                        Text("/")
                            .font(Theme.mono(15, weight: .semibold))
                            .foregroundStyle(Theme.ink)
                            .frame(width: 32, height: 32)
                            .background(Theme.fill, in: .circle)
                    }
                    .buttonStyle(.plain)
                    .disabled(!model.draft.isEmpty || model.commands.isEmpty || model.ended != nil)
                    .opacity(model.draft.isEmpty && !model.commands.isEmpty ? 1 : 0.4)
                    .accessibilityLabel("Commands")

                    if !model.models.isEmpty {
                        Menu {
                            ForEach(model.models, id: \.model) { choice in
                                Button {
                                    model.selectModel(choice.model)
                                } label: {
                                    if choice.model == model.model {
                                        Label(choice.display, systemImage: "checkmark")
                                    } else {
                                        Text(choice.display)
                                    }
                                }
                            }
                        } label: {
                            Image(systemName: "cpu")
                                .font(.system(size: 14, weight: .semibold))
                                .foregroundStyle(Theme.ink)
                                .frame(width: 32, height: 32)
                                .background(Theme.fill, in: .circle)
                        }
                        .menuOrder(.fixed)
                        .disabled(!model.attached || model.ended != nil)
                        .accessibilityLabel(tr("Model") + ": " + model.modelLabel)
                    }

                    if !model.efforts.isEmpty {
                        Menu {
                            ForEach(model.efforts, id: \.self) { effort in
                                Button {
                                    model.selectEffort(effort)
                                } label: {
                                    if effort == model.effort {
                                        Label(effort.capitalized, systemImage: "checkmark")
                                    } else {
                                        Text(effort.capitalized)
                                    }
                                }
                            }
                        } label: {
                            ChipLabel(text: model.effort?.capitalized ?? tr("Effort"))
                        }
                        .menuOrder(.fixed)
                        .disabled(!model.attached || model.ended != nil)
                        .accessibilityLabel(tr("Effort") + ": " + (model.effort?.capitalized ?? tr("Default")))
                    }

                    if !model.skills.isEmpty {
                        Menu {
                            ForEach(model.skills, id: \.path) { skill in
                                Button {
                                    model.insertSkill(skill)
                                } label: {
                                    Text(skill.title)
                                    Text(skill.detail)
                                }
                                .disabled(!skill.enabled)
                            }
                        } label: { ChipLabel(text: tr("Skills")) }
                    }

                    Spacer()

                    Button(action: model.primaryAction) {
                        Image(systemName: model.showsStop ? "stop.fill" : "arrow.up")
                            .font(.system(size: model.showsStop ? 12 : 15, weight: .bold))
                            .foregroundStyle(Theme.onAccent)
                            .frame(width: 36, height: 36)
                            .background(Theme.accent.opacity(model.primaryEnabled ? 1 : 0.35), in: .circle)
                            .contentTransition(.symbolEffect(.replace))
                    }
                    .buttonStyle(.plain)
                    .disabled(!model.primaryEnabled)
                    .accessibilityLabel(model.showsStop ? "Interrupt" : "Send")
                }
            }
            .padding(.leading, 16)
            .padding(.trailing, 10)
            .padding(.top, 12)
            .padding(.bottom, 10)
            .glassRounded(28)

            HStack {
                Text(model.phaseLine).lineLimit(1)

                Spacer()

                if let context = model.contextLine { Text(context) }
            }
            .font(Theme.mono(10.5))
            .foregroundStyle(Theme.secondary)
            .padding(.horizontal, 10)
        }
        .padding(.horizontal, 12)
        .padding(.bottom, 4)
        .animation(.snappy, value: model.queue.count)
        .animation(.snappy, value: model.suggestions(skillsInSlash: codexSkillsInSlash).isEmpty)
    }
}

struct SuggestionRow: View {
    let suggestion: ComposerSuggestion

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(suggestion.label)
                .font(Theme.mono(13.5, weight: .semibold))
                .foregroundStyle(Theme.ink)

            if !suggestion.detail.isEmpty {
                Text(suggestion.detail)
                    .font(.system(size: 12.5))
                    .foregroundStyle(Theme.secondary)
                    .lineLimit(1)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, 16)
        .padding(.vertical, 7)
        .opacity(suggestion.enabled ? 1 : 0.4)
        .contentShape(.rect)
    }
}

struct ChipLabel: View {
    let text: String

    var body: some View {
        HStack(spacing: 4) {
            Text(text).lineLimit(1)

            Image(systemName: "chevron.down")
                .font(.system(size: 9, weight: .semibold))
                .foregroundStyle(Theme.tertiary)
        }
        .font(Theme.mono(12.5))
        .foregroundStyle(Theme.ink)
        .padding(.horizontal, 10)
        .frame(height: 32)
        .background(Theme.fill, in: .capsule)
    }
}
