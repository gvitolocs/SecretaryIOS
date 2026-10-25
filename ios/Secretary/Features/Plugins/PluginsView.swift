import SwiftUI

// The Plugins tab (owner, 2026-10-24): everything that is not Chat, Shopping or Training +
// Weight is a plugin card here, instead of another tab. A new feature adds one entry to
// `Plugin.all` and, when it pushes a screen, a navigationDestination below.

struct Plugin: Identifiable {
    let id: String
    let icon: String
    let title: LocalizedStringKey
    var detail: String?
    /// Shown in the accent colour while something is running (a recording, a backup).
    var isActive = false
    let open: @MainActor () -> Void

    @MainActor static func all(_ app: AppModel) -> [Plugin] {
        [
            Plugin(id: "gallery", icon: "photo.on.rectangle.angled", title: "Gallery",
                   detail: app.backup.backedUp > 0 ? String(localized: "\(app.backup.backedUp) backed up") : nil,
                   isActive: app.backup.remaining > 0) { app.showGallery = true },
            Plugin(id: "memos", icon: app.memos.recorder.isActive ? "waveform.circle.fill" : "waveform",
                   title: "Voice memos",
                   detail: app.memos.recorder.isActive ? String(localized: "Recording") : nil,
                   isActive: app.memos.recorder.isActive) { app.showMemos = true },
            Plugin(id: "media", icon: "play.rectangle", title: "Media",
                   detail: String(localized: "Films, music and audio")) { app.showMedia = true },
            Plugin(id: "pokemon", icon: "gamecontroller", title: "Pokémon",
                   detail: String(localized: "Lucina's game")) { app.showPokemon = true },
            Plugin(id: "notes", icon: "doc.text", title: "Notes") { app.showNotes = true },
            Plugin(id: "tasks", icon: "checkmark.circle", title: "Tasks") { app.showTasks = true },
            Plugin(id: "wardrobe", icon: "tshirt", title: "Wardrobe",
                   detail: String(localized: "Lucina's outfits")) { app.wardrobe.presented = true },
        ]
    }
}

struct PluginsView: View {
    @Environment(AppModel.self) private var app

    var body: some View {
        @Bindable var app = app
        ScrollView {
            StatGrid {
                ForEach(Plugin.all(app)) { plugin in
                    PluginCard(plugin: plugin)
                }
            }
            .padding(.horizontal, LucinaTokens.Space.gutter)
            .padding(.top, LucinaTokens.Space.sm)
            .padding(.bottom, LucinaTokens.Space.xxl)
        }
        .lucinaScreen("Plugins")
        .navigationDestination(isPresented: $app.showGallery) { GalleryScreen() }
        .navigationDestination(isPresented: $app.showMemos) { MemosView().environment(app.memos) }
        .navigationDestination(isPresented: $app.showMedia) { MediaLibraryView() }
        .navigationDestination(isPresented: $app.showNotes) { NotesView() }
        .navigationDestination(isPresented: $app.showTasks) { TasksView() }
        .navigationDestination(isPresented: $app.showPokemon) { PokemonView() }
    }
}

/// One plugin: an icon in an accent circle, the name, and a status line.
struct PluginCard: View {
    let plugin: Plugin

    var body: some View {
        Button(action: plugin.open) {
            Card(spacing: 10) {
                Image(systemName: plugin.icon)
                    .font(.title3.weight(.semibold))
                    .foregroundStyle(plugin.isActive ? Theme.onAccent : Theme.accent)
                    .frame(width: 44, height: 44)
                    .background(plugin.isActive ? Theme.accentFill : Theme.accentSoft, in: Circle())
                    .accessibilityHidden(true)
                Text(plugin.title)
                    .font(LucinaTokens.Fonts.headline)
                    .foregroundStyle(Theme.text)
                    .lineLimit(1)
                    .minimumScaleFactor(0.8)
                Text(verbatim: plugin.detail ?? " ")
                    .font(LucinaTokens.Fonts.caption2)
                    .foregroundStyle(Theme.textSecondary)
                    .lineLimit(1)
            }
            .contentShape(RoundedRectangle(cornerRadius: Theme.cornerRadius, style: .continuous))
        }
        .buttonStyle(.plain)
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(.isButton)
        .accessibilityIdentifier("plugin-\(plugin.id)")
    }
}
