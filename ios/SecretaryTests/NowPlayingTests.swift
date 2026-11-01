import MediaPlayer
import XCTest
@testable import Secretary

/// The Lock Screen media widget exists only while the film/audio player plays: never at launch,
/// never while a voice memo records.
@MainActor
final class NowPlayingTests: XCTestCase {
    func testThePlayerRegistersNothingUntilItPlays() {
        NowPlayingSession.deactivate()
        let file = FileManager.default.temporaryDirectory.appendingPathComponent("player-\(UUID().uuidString).json")
        defer { try? FileManager.default.removeItem(at: file) }
        let player = Secretary.MediaPlayer(api: { fatalError("no network in this test") }, coreFile: file)
        XCTAssertFalse(NowPlayingSession.isActive, "remote commands registered at launch")
        XCTAssertNil(MPNowPlayingInfoCenter.default().nowPlayingInfo)
        player.stop()
        XCTAssertFalse(NowPlayingSession.isActive)
    }

    func testDeactivateRemovesTargetsAndInfo() {
        NowPlayingSession.deactivate()
        var registered = 0
        let register: (MPRemoteCommandCenter) -> [(command: MPRemoteCommand, target: Any)] = { center in
            registered += 1
            return [(center.playCommand, center.playCommand.addTarget { _ in .success })]
        }
        NowPlayingSession.activate(register)
        NowPlayingSession.activate(register)
        XCTAssertEqual(registered, 1, "registered once while playing")
        XCTAssertTrue(NowPlayingSession.isActive)
        NowPlayingSession.update([MPMediaItemPropertyTitle: "Film"])
        XCTAssertNotNil(MPNowPlayingInfoCenter.default().nowPlayingInfo)

        // What a memo recording (or the player's stop) does.
        NowPlayingSession.deactivate()
        XCTAssertFalse(NowPlayingSession.isActive)
        XCTAssertFalse(MPRemoteCommandCenter.shared().playCommand.isEnabled)
        XCTAssertNil(MPNowPlayingInfoCenter.default().nowPlayingInfo)

        // A paused player's late update does not bring the widget back.
        NowPlayingSession.update([MPMediaItemPropertyTitle: "Film"])
        XCTAssertNil(MPNowPlayingInfoCenter.default().nowPlayingInfo)
    }
}
