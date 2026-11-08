import SwiftUI

/// The Lucina tab: her 3D stage (the shared web view), the camera presets, the studio menu,
/// the wardrobe, voice and mute. The stage, her model and the wardrobe load the first time this
/// tab opens, never at launch; the Chat tab has no avatar at all. Off screen the page stops
/// drawing (WebKit pauses a web view outside the window), and a memory warning releases it.
struct LucinaTab: View {
    @Environment(AppModel.self) private var app
    @Environment(\.horizontalSizeClass) private var horizontalSize
    @Environment(\.verticalSizeClass) private var verticalSize

    var body: some View {
        let controller = app.lucina
        Group {
            if !controller.enabled {
                hidden(controller)
            } else if controller.awake {
                if horizontalSize == .regular || verticalSize == .compact {
                    // iPad, or landscape: the stage beside the controls.
                    HStack(spacing: 0) {
                        stage(controller)
                        Divider()
                        VStack(spacing: LucinaTokens.Space.md) {
                            Spacer(minLength: 0)
                            LucinaViewBar(controller: controller)
                            LucinaControls(controller: controller)
                            Spacer(minLength: 0)
                        }
                        .padding(LucinaTokens.Space.lg)
                        .frame(width: 340)
                    }
                } else {
                    VStack(spacing: LucinaTokens.Space.md) {
                        stage(controller)
                        LucinaViewBar(controller: controller)
                        LucinaControls(controller: controller)
                            .padding(.bottom, LucinaTokens.Space.lg)
                    }
                }
            } else {
                // The frame before onAppear wakes her.
                Color.clear
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.page.ignoresSafeArea())
        .navigationTitle("Lucina")
        .navigationBarTitleDisplayMode(.inline)
        .onAppear { controller.setVisible(true) }
        .onDisappear {
            controller.setVisible(false)
            // Face tracking only runs while she is on screen.
            LucinaFaceTracker.shared.stop()
        }
        // Her outfits and settings are only needed here (WardrobeStore throttles to one load in 30 s).
        .task { await app.wardrobe.load() }
    }

    private func stage(_ controller: LucinaStageController) -> some View {
        StageHost(controller: controller, framing: .full)
            // A released web view (memory warning, WebKit dropped the page) gets a fresh host.
            .id(controller.generation)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .overlay {
                if controller.phase == .loading || controller.phase == .idle {
                    ProgressView().controlSize(.large)
                }
            }
            .accessibilityIdentifier("lucinaStage")
    }

    private func hidden(_ controller: LucinaStageController) -> some View {
        VStack(spacing: LucinaTokens.Space.md) {
            Image(systemName: "person.crop.circle.badge.xmark")
                .font(.system(size: 44))
                .foregroundStyle(Theme.accent)
                .accessibilityHidden(true)
            Text("Lucina's 3D stage is off.")
                .font(LucinaTokens.Fonts.headline)
            Button("Show Lucina") { controller.enabled = true }
                .buttonStyle(.borderedProminent)
                .tint(Theme.accentFill)
                .accessibilityIdentifier("lucinaShow")
        }
        .padding(LucinaTokens.Space.xxl)
    }
}
