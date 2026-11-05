import SwiftUI

struct ScrollToBottomButton: View {
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Image(systemName: "arrow.down")
                .font(.system(size: 17, weight: .semibold))
                .foregroundStyle(.primary)
                .frame(width: 44, height: 44)
                .glassCapsule(interactive: true)
        }
        .buttonStyle(.plain)
        .accessibilityLabel(Text("Jump to bottom"))
        .padding(.bottom, 12)
    }
}
