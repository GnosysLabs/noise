import SwiftUI

@main
struct NoiseApp: App {
    @StateObject private var model = AppModel()

    var body: some Scene {
        WindowGroup {
            ContentView()
                .environmentObject(model)
                .frame(minWidth: 920, minHeight: 620)
        }
        .windowStyle(.hiddenTitleBar)
        .defaultSize(width: 1120, height: 740)
        .commands {
            CommandGroup(replacing: .newItem) {
                Button("make noise") {
                    model.presentedSheet = .make
                }
                .keyboardShortcut("n", modifiers: .command)
                .disabled(model.summary == nil)

                Button("tune in") {
                    model.presentedSheet = .join
                }
                .keyboardShortcut("j", modifiers: .command)
                .disabled(model.summary == nil)
            }
        }
    }
}
