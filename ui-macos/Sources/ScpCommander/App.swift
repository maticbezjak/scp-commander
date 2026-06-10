import SwiftUI

@main
struct ScpCommanderApp: App {
    @StateObject private var state = AppState()

    var body: some Scene {
        WindowGroup("SCP Commander") {
            ContentView()
                .environmentObject(state)
        }
        .windowStyle(.titleBar)
    }
}
