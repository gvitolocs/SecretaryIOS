import MediaPlayer

/// The Lock Screen's media player: Now Playing info and the remote commands (play, pause, skip).
/// iOS shows the media widget for an app that has either, so they exist only while the film/audio
/// player plays an item (`MediaPlayer`), and go when it stops or when a voice memo starts
/// recording: then only the memo's Live Activity is on the Lock Screen.
@MainActor
enum NowPlayingSession {
    private static var targets: [(command: MPRemoteCommand, target: Any)] = []

    /// The remote commands are registered.
    static var isActive: Bool { !targets.isEmpty }

    /// Registers the player's remote commands once; `register` adds the targets and returns them.
    static func activate(_ register: (MPRemoteCommandCenter) -> [(command: MPRemoteCommand, target: Any)]) {
        guard targets.isEmpty else { return }
        targets = register(MPRemoteCommandCenter.shared())
        for (command, _) in targets { command.isEnabled = true }
    }

    /// Removes every remote command target and the Now Playing info.
    static func deactivate() {
        for (command, target) in targets {
            command.removeTarget(target)
            command.isEnabled = false
        }
        targets = []
        MPNowPlayingInfoCenter.default().nowPlayingInfo = nil
    }

    /// Sets the Now Playing info, only while the commands are registered (nil clears it).
    static func update(_ info: [String: Any]?) {
        MPNowPlayingInfoCenter.default().nowPlayingInfo = isActive ? info : nil
    }
}
