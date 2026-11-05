import CoreText
import SwiftUI

@main
struct NiumaTermApp: App {
    @UIApplicationDelegateAdaptor private var delegate: AppDelegate
    @State private var app = AppModel()
    @Environment(\.scenePhase) private var scenePhase
    @AppStorage("appearance") private var appearance = AppAppearance.system

    init() {
        FontRegistrar.registerBundledFonts()
    }

    var body: some Scene {
        WindowGroup {
            HostListView()
                .environment(app)
                .tint(Theme.accent)
                .modifier(AppLocaleEnvironment())
                .onChange(of: appearance, initial: true) { _, appearance in
                    appearance.apply()
                }
                .task {
                    delegate.app = app

                    await app.startPush()
                }
        }
        // iOS suspends a background app within seconds, so the links wind
        // down there and come back, with fresh session lists, on return
        // (design doc §6).
        .onChange(of: scenePhase, initial: true) { _, phase in
            app.setApplicationPhase(phase)
        }
    }
}

/// Registers every .ttf/.otf in the bundle so no UIAppFonts plist entry is needed.
/// Drop JetBrainsMonoNerdFontMono-*.ttf into NiumaTerm/Resources/Fonts to enable it (design doc §8.2).
enum FontRegistrar {
    static func registerBundledFonts() {
        let urls = (Bundle.main.urls(forResourcesWithExtension: "ttf", subdirectory: nil) ?? [])
            + (Bundle.main.urls(forResourcesWithExtension: "otf", subdirectory: nil) ?? [])

        for url in urls {
            _ = CTFontManagerRegisterFontsForURL(url as CFURL, .process, nil)
        }
    }
}
