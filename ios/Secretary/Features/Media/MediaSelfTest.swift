#if DEBUG || SCREENSHOTS
import Foundation
import os

private let log = Logger(subsystem: "tech.getsecretary.app", category: "MediaSelfTest")

/// `-SecretaryInitialTab media -SecretaryMediaSelfTest <title words | any>`: the player on the
/// iPhone without UI tests (XCUITest can't drive iOS 27 from Xcode 26.6). Always muted.
/// Plays the first video whose title has the words, checks the position moves, loads a subtitle
/// track (text first), seeks to a cue, leaves and reopens to check the resume position, then puts
/// the saved position back to what it was. Each step logs one `MediaSelfTest …` line.
@MainActor
enum MediaSelfTest {
    private static var ran = false

    static func runIfRequested(media: MediaPlayer, items: [MediaItem]) async {
        guard !ran, let query = UserDefaults.standard.string(forKey: "SecretaryMediaSelfTest"), !items.isEmpty else { return }
        ran = true
        guard let item = items.first(where: { query == "any" || $0.title.localizedCaseInsensitiveContains(query) }) else {
            log.notice("MediaSelfTest no item matches \(query, privacy: .public)")
            return
        }
        media.player.isMuted = true
        let saved = media.savedPositionMs(id: item.id)
        log.notice("MediaSelfTest start title=\(item.title, privacy: .public) saved_ms=\(saved ?? -1) muted=\(media.player.isMuted)")

        media.play([item], startAt: 0)
        media.presented = true
        let playing = await wait(90) { media.isPlaying && media.position > 0.5 }
        log.notice("MediaSelfTest playing ok=\(playing) pos=\(String(format: "%.1f", media.position), privacy: .public) muted=\(media.player.isMuted)")
        let p0 = media.position
        try? await Task.sleep(for: .seconds(4))
        let advanced = media.position - p0 > 2
        log.notice("MediaSelfTest advances ok=\(advanced) from=\(String(format: "%.1f", p0), privacy: .public) to=\(String(format: "%.1f", media.position), privacy: .public)")

        var subtitles = true
        var target = min(90, max(10, media.duration * 0.3))
        let tracks = media.subtitleTracks
        if let track = media.subtitle ?? tracks.first(where: { !$0.isImage }) ?? tracks.first {
            if media.subtitle?.id != track.id { media.selectSubtitle(track, remember: false) }
            subtitles = await wait(240) { media.cueCount > 0 }
            if let start = media.cueStart(after: 60) { target = start + 0.5 }
            log.notice("MediaSelfTest subtitles ok=\(subtitles) track=\(track.label, privacy: .public) image=\(track.isImage) cues=\(media.cueCount) of \(tracks.count) tracks")
        } else {
            log.notice("MediaSelfTest subtitles none for this item")
        }

        media.seek(to: target)
        let seeked = await wait(15) { abs(media.position - target) < 2 && media.isPlaying }
        try? await Task.sleep(for: .seconds(1))
        log.notice("MediaSelfTest seek ok=\(seeked) target=\(String(format: "%.1f", target), privacy: .public) pos=\(String(format: "%.1f", media.position), privacy: .public) cue=\(media.cueText ?? "-", privacy: .public)")
        // Time for a screenshot of the cue on screen.
        try? await Task.sleep(for: .seconds(5))

        let before = media.position
        media.stop()
        await media.pushProgress()
        try? await Task.sleep(for: .seconds(1))
        media.play([item], startAt: 0)
        let reopened = await wait(90) { media.isPlaying && media.position > 0.5 }
        let resumed = reopened && abs(media.position - before) < 5
        log.notice("MediaSelfTest resume ok=\(resumed) left=\(String(format: "%.1f", before), privacy: .public) back=\(String(format: "%.1f", media.position), privacy: .public)")

        // Leave the owner's saved position as it was.
        media.seek(to: TimeInterval(saved ?? 0) / 1000)
        try? await Task.sleep(for: .seconds(1))
        media.player.pause()
        media.stop()
        await media.pushProgress()
        media.presented = false
        let pass = playing && advanced && subtitles && seeked && resumed
        log.notice("MediaSelfTest done pass=\(pass) restored_ms=\(media.savedPositionMs(id: item.id) ?? -1) muted=\(media.player.isMuted)")
    }

    private static func wait(_ seconds: Double, until done: () -> Bool) async -> Bool {
        let deadline = Date().addingTimeInterval(seconds)
        while Date() < deadline {
            if done() { return true }
            try? await Task.sleep(for: .milliseconds(250))
        }
        return done()
    }
}
#endif
