import SwiftUI
import UIKit

// Lucina's Pokémon Red run (plugin card "Pokémon"): the live screen, her team, badges, objective and latest
// notes, from /v1/game/* (secretary-server → Lucina → the game server on nezopt). Play and Pause are the
// owner's; the chat is where she reports and takes advice.

struct GameMon: Decodable, Equatable, Sendable, Identifiable {
    var id: String { name }
    let name: String
    let species: String?
    let level: Int
    let hp: Int
    let maxHp: Int

    private enum CodingKeys: String, CodingKey { case name, species, level, hp, maxHp = "max_hp" }
}

struct GameState: Decodable, Equatable, Sendable {
    let running: Bool
    let mapName: String
    let mode: String?
    let badges: Int
    let objective: String?
    let events: [String]
    let party: [GameMon]

    private enum CodingKeys: String, CodingKey { case running, mapName = "map_name", mode, badges, objective, events, party }

    /// "ROCKET_HIDEOUT_B4F" → "Rocket Hideout B4F".
    var place: String {
        mapName.lowercased().split(separator: "_").map { word -> String in
            let w = String(word)
            return w.range(of: #"^b?\d+f$"#, options: .regularExpression) != nil ? w.uppercased() : w.prefix(1).uppercased() + w.dropFirst()
        }.joined(separator: " ")
    }

    /// Her own notes, newest first, without the server's housekeeping lines.
    var notes: [String] {
        events.filter { !$0.hasPrefix("Game server restarted") && !$0.hasPrefix("Lucina is playing") }.reversed()
    }
}

extension SecretaryAPI {
    func gameState() async throws -> GameState {
        try await client.get("/v1/game/state")
    }

    /// The current screen (160×144 PNG).
    func gameScreenshot() async throws -> Data {
        try await client.send("GET", "/v1/game/screenshot", body: nil, headers: ["Accept": "image/png"]).data
    }

    /// Start (`true`) or pause the run.
    func setGameRunning(_ running: Bool) async throws {
        struct Body: Encodable { let running: Bool }
        struct Reply: Decodable {}
        let _: Reply = try await client.send("POST", "/v1/game/control", body: Body(running: running))
    }
}

struct PokemonView: View {
    @Environment(AppModel.self) private var app
    @State private var state: GameState?
    @State private var screen: UIImage?
    @State private var error: String?
    @State private var busy = false

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: LucinaTokens.Space.lg) {
                Card(spacing: LucinaTokens.Space.md) {
                    ZStack {
                        Color.black
                        if let screen {
                            Image(uiImage: screen)
                                .interpolation(.none)
                                .resizable()
                                .aspectRatio(160.0 / 144.0, contentMode: .fit)
                                .accessibilityLabel(Text("Lucina's game screen"))
                        } else {
                            ProgressView().tint(.white)
                        }
                    }
                    .aspectRatio(160.0 / 144.0, contentMode: .fit)
                    .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))

                    if let state {
                        HStack(spacing: LucinaTokens.Space.sm) {
                            Circle()
                                .fill(state.running ? Theme.positive : Theme.textTertiary)
                                .frame(width: 8, height: 8)
                                .accessibilityHidden(true)
                            Text(state.running ? "Playing" : "Paused")
                                .font(LucinaTokens.Fonts.headline)
                            Text("·").foregroundStyle(Theme.textTertiary)
                            Text(verbatim: state.place)
                                .font(LucinaTokens.Fonts.subheadline)
                                .foregroundStyle(Theme.textSecondary)
                                .lineLimit(1)
                            Spacer()
                            Label(String(state.badges), systemImage: "rosette")
                                .font(LucinaTokens.Fonts.subheadline)
                                .foregroundStyle(Theme.textSecondary)
                                .accessibilityLabel(Text("\(state.badges) badges"))
                        }
                        if let objective = state.objective, !objective.isEmpty {
                            Text(verbatim: objective)
                                .font(LucinaTokens.Fonts.callout)
                                .foregroundStyle(Theme.text)
                        }
                    }
                    if let error {
                        Text(verbatim: error)
                            .font(LucinaTokens.Fonts.footnote)
                            .foregroundStyle(Theme.negative)
                    }
                    HStack(spacing: LucinaTokens.Space.sm) {
                        Button {
                            Task { await toggle() }
                        } label: {
                            Label(state?.running == true ? "Pause" : "Play",
                                  systemImage: state?.running == true ? "pause.fill" : "play.fill")
                                .frame(maxWidth: .infinity)
                        }
                        .buttonStyle(.bordered)
                        .disabled(state == nil || busy)

                        Button {
                            app.chat.requestPrefill(String(localized: "How is the Pokémon game going?"))
                            app.showPokemon = false
                            app.selectedTab = .chat
                        } label: {
                            Label("Ask Lucina", systemImage: "bubble.left")
                                .frame(maxWidth: .infinity)
                        }
                        .buttonStyle(.borderedProminent)
                        .tint(Theme.accent)
                    }
                }

                if let state, !state.party.isEmpty {
                    SectionHeader("Team")
                    Card(spacing: LucinaTokens.Space.md) {
                        ForEach(state.party) { mon in
                            VStack(alignment: .leading, spacing: LucinaTokens.Space.xs) {
                                HStack {
                                    Text(verbatim: mon.name).font(LucinaTokens.Fonts.headline)
                                    if let species = mon.species, species != mon.name {
                                        Text(verbatim: species.capitalized)
                                            .font(LucinaTokens.Fonts.footnote)
                                            .foregroundStyle(Theme.textSecondary)
                                    }
                                    Spacer()
                                    Text("Lv \(mon.level)").font(LucinaTokens.Fonts.subheadline)
                                }
                                HStack(spacing: LucinaTokens.Space.sm) {
                                    ProgressView(value: Double(mon.hp), total: Double(max(mon.maxHp, 1)))
                                        .tint(hpColor(mon))
                                    Text(verbatim: "\(mon.hp)/\(mon.maxHp)")
                                        .font(LucinaTokens.Fonts.caption2)
                                        .monospacedDigit()
                                        .foregroundStyle(Theme.textSecondary)
                                }
                                .accessibilityElement(children: .ignore)
                                .accessibilityLabel(Text("HP \(mon.hp) of \(mon.maxHp)"))
                            }
                        }
                    }
                }

                if let state, !state.notes.isEmpty {
                    SectionHeader("Her notes")
                    Card(spacing: LucinaTokens.Space.sm) {
                        ForEach(Array(state.notes.prefix(8).enumerated()), id: \.offset) { _, note in
                            Text(verbatim: note)
                                .font(LucinaTokens.Fonts.callout)
                                .foregroundStyle(Theme.text)
                                .frame(maxWidth: .infinity, alignment: .leading)
                        }
                    }
                }
            }
            .padding(.horizontal, LucinaTokens.Space.gutter)
            .padding(.top, LucinaTokens.Space.sm)
            .padding(.bottom, LucinaTokens.Space.xxl)
        }
        .lucinaScreen("Pokémon")
        // While the screen is visible: the picture about once a second, the state every third tick.
        .task {
            var tick = 0
            while !Task.isCancelled {
                await refresh(stateToo: tick % 3 == 0)
                tick += 1
                try? await Task.sleep(for: .seconds(1))
            }
        }
    }

    private func hpColor(_ mon: GameMon) -> Color {
        let ratio = Double(mon.hp) / Double(max(mon.maxHp, 1))
        return ratio > 0.5 ? Theme.positive : ratio > 0.2 ? Theme.warning : Theme.negative
    }

    private func refresh(stateToo: Bool) async {
        do {
            if stateToo { state = try await app.api.gameState() }
            if let image = UIImage(data: try await app.api.gameScreenshot()) { screen = image }
            error = nil
        } catch {
            self.error = String(localized: "Lucina's game is not reachable right now.")
        }
    }

    private func toggle() async {
        guard let state else { return }
        busy = true
        defer { busy = false }
        do {
            try await app.api.setGameRunning(!state.running)
            self.state = try await app.api.gameState()
        } catch {
            self.error = String(localized: "Could not change the game right now.")
        }
    }
}
