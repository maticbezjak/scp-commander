import SwiftUI

@main
struct ScpCommanderApp: App {
    @StateObject private var state = AppState()

    var body: some Scene {
        WindowGroup("SCP Commander") {
            ContentView()
                .environmentObject(state)
                .onOpenURL { url in
                    Task { @MainActor in state.openURL(url) }
                }
        }
        .windowStyle(.titleBar)
    }
}
