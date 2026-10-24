# Lucina design tokens

Every Lucina app (iOS, web, Android, macOS, Windows) shares one look: the one of the
iOS **Weight**, **Settings** and **Chat** tabs, picked by the owner on 2026-10-24.

- Black page (`bg`), dark-grey rounded cards (`card`, radius 18), periwinkle accent
  (`accent` #7F8FE0 dark / #4A5BB5 light; `accentFill` #4A5BB5 under white text).
- System font (SF Pro on Apple), large bold screen titles, `caption` labels with an icon over
  a `title2` value in stat cards.
- Pill rows (capsule on `card`, a filled `accentFill` capsule button).
- Charts: an accent line (2.5 pt) over faint points, a dashed `warning` goal rule.
- A floating glass tab bar; the active tab in the accent colour.

## Files

| File | What |
|---|---|
| `design/tokens.json` | The source. Edit only this. |
| `tools/design-tokens/` | The generator (Rust, standalone crate). |
| `ios/Secretary/Shared/LucinaTokens.swift` | Generated: SwiftUI (iOS; macOS through AppKit). |
| `web/src/styles/lucina-tokens.css` | Generated: CSS custom properties `--lu-*`. |
| `design/generated/compose/LucinaTokens.kt` | Generated: Jetpack Compose (Compose Desktop on Windows; Android if it moves to Compose). |
| `design/generated/android/LucinaTokens.kt` | Generated: framework-only Kotlin (ARGB `Int`s, dp/sp `Float`s, no imports); also written to `android/common/LucinaTokens.kt` where that folder exists (branch lucina-android). |

```sh
cargo run --manifest-path tools/design-tokens/Cargo.toml             # regenerate
cargo run --manifest-path tools/design-tokens/Cargo.toml -- --check  # fails if a file is stale
```

## Using them

**iOS / macOS.** Use `Theme` (Components.swift) and the components in
`ios/Secretary/Shared/DesignSystem.swift`, not raw colours, fonts or radii:
`.lucinaScreen("Title")` or `.lucinaList()` on the screen, `Card`, `StatCard` in a `StatGrid`,
`ChartCard` + `LucinaChartStyle`, `PillInputRow` / `PillButtonRow`, `SectionHeader`,
`ChipButton`, `SectionBar`, `PrimaryButton`, and `.lucinaSheet()` on sheets. Keep the
system `TabView` (it is the floating glass bar) and `NavigationStack` large titles.

**Web.** `lucina-tokens.css` is imported first; `tokens.css` maps the older names
(`--bg`, `--surface`, `--accent`, …) onto the `--lu-*` values, so new rules should use
`--lu-*` directly.

**Android.** The apps are framework-only Kotlin: `android/common/LucinaTokens.kt` gives
`LucinaTokens.Dark` / `.Light` colours, `Radius`, `Space`, `TextSize`, `TextWeight`, `LineHeight`.
A Compose app would use `design/generated/compose/LucinaTokens.kt` instead.

Semantic colours: `positive` (gains you want, done), `negative` (errors, destructive,
overdue), `warning` (goal rules, due soon, clearance), `info` (secondary series).
