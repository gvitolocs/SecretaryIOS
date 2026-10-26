import AVKit
import SwiftUI

// The player for the owner's films and audio (the Films plugin is FilmsView).
// Video plays in AVPlayerViewController (AirPlay, Picture in Picture, native scrubbing);
// audio gets its own screen; both continue in the background and on the lock screen.

/// A poster or thumbnail from a tokenised URL (no auth header needed).
struct Poster: View {
    @Environment(MediaPlayer.self) private var media
    let path: String?
    let kind: String

    var body: some View {
        AsyncImage(url: path.flatMap { media.api().absolute($0) }) { phase in
            if let image = phase.image {
                image.resizable().scaledToFill()
            } else {
                ZStack {
                    Rectangle().fill(Theme.field)
                    Image(systemName: kind == "audio" ? "waveform" : kind == "photo" ? "photo" : "film")
                        .foregroundStyle(Theme.textTertiary)
                }
            }
        }
    }
}

/// The bar above the bottom edge while something plays and the player is closed.
struct MiniPlayer: View {
    @Environment(MediaPlayer.self) private var media

    var body: some View {
        if let item = media.current {
            HStack(spacing: 12) {
                Poster(path: item.posterUrl, kind: item.kind)
                    .frame(width: 44, height: 44)
                    .clipShape(RoundedRectangle(cornerRadius: LucinaTokens.Radius.field))
                VStack(alignment: .leading, spacing: 2) {
                    Text(item.title).font(LucinaTokens.Fonts.subheadline.weight(.medium)).foregroundStyle(Theme.text).lineLimit(1)
                    Text("\(MediaFormat.time(media.position)) / \(MediaFormat.time(media.duration))")
                        .font(LucinaTokens.Fonts.caption2.monospacedDigit()).foregroundStyle(Theme.textSecondary)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .contentShape(Rectangle())
                .onTapGesture { media.presented = true }
                Button { media.togglePlay() } label: {
                    Image(systemName: media.isPlaying ? "pause.fill" : "play.fill").font(.title3).foregroundStyle(Theme.accent)
                }
                .accessibilityLabel(media.isPlaying ? "Pause" : "Play")
                Button { media.stop() } label: { Image(systemName: "xmark").font(.body.weight(.semibold)) }
                    .foregroundStyle(Theme.textSecondary)
                    .accessibilityLabel("Close")
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .background(Theme.cardRaised, in: RoundedRectangle(cornerRadius: Theme.cornerRadius))
            .padding(.horizontal, 12)
            .padding(.bottom, 6)
        }
    }
}

struct MediaPlayerScreen: View {
    @Environment(MediaPlayer.self) private var media
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        Group {
            if media.current?.isVideo == true {
                videoScreen
            } else {
                AudioPlayerScreen()
            }
        }
        .onAppear { if media.current?.isVideo == true { OrientationLock.set(.allButUpsideDown) } }
        .onDisappear { OrientationLock.set(.portrait) }
    }

    private var videoScreen: some View {
        ZStack(alignment: .top) {
            Color.black.ignoresSafeArea()
            VideoSurface(media: media).ignoresSafeArea()
            if media.loading {
                ProgressView().tint(.white).controlSize(.large).frame(maxHeight: .infinity)
            }
            if let error = media.error {
                Text(error).font(.callout).foregroundStyle(.white).padding().background(.black.opacity(0.7), in: RoundedRectangle(cornerRadius: 10))
                    .frame(maxHeight: .infinity)
            }
            HStack {
                Button { dismiss() } label: {
                    Image(systemName: "chevron.down").font(.body.weight(.semibold)).padding(10)
                }
                .accessibilityLabel("Close")
                .accessibilityIdentifier("closePlayer")
                Spacer()
                SubtitleMenu()
            }
            .foregroundStyle(.white)
            .background(.black.opacity(0.35), in: Capsule())
            .padding(.horizontal, 16)
            // Below AVKit's own top controls (Picture in Picture, AirPlay, volume).
            .padding(.top, 52)
        }
    }
}

private struct SubtitleMenu: View {
    @Environment(MediaPlayer.self) private var media

    var body: some View {
        Menu {
            Section("Subtitles") {
                Button { media.selectSubtitle(nil) } label: {
                    if media.subtitle == nil { Label("Off", systemImage: "checkmark") } else { Text("Off") }
                }
                ForEach(media.subtitleTracks) { track in
                    Button { media.selectSubtitle(track) } label: {
                        if media.subtitle?.id == track.id {
                            Label(track.displayName, systemImage: "checkmark")
                        } else {
                            Text(track.displayName)
                        }
                        Text(track.detail)
                    }
                    .accessibilityIdentifier("subtitle-\(track.id)")
                }
                if let hidden = media.info?.bitmapSubtitles, hidden > 0 {
                    Text("\(hidden) picture subtitle tracks need OCR on the server")
                }
            }
            Menu("Speed") {
                ForEach([Float(0.75), 1, 1.25, 1.5, 2], id: \.self) { r in
                    Button { media.setRate(r) } label: {
                        let text = r == 1 ? "1×" : "\(r.formatted())×"
                        if media.rate == r { Label(text, systemImage: "checkmark") } else { Text(text) }
                    }
                }
            }
        } label: {
            Image(systemName: media.subtitle == nil ? "captions.bubble" : "captions.bubble.fill").font(.body.weight(.semibold)).padding(10)
        }
        .accessibilityLabel("Subtitles and speed")
        .accessibilityIdentifier("subtitleMenu")
    }
}

/// AVPlayerViewController with the subtitle overlay in its content overlay view.
private struct VideoSurface: UIViewControllerRepresentable {
    let media: MediaPlayer

    func makeUIViewController(context: Context) -> AVPlayerViewController {
        let controller = AVPlayerViewController()
        controller.player = media.player
        controller.allowsPictureInPicturePlayback = true
        controller.canStartPictureInPictureAutomaticallyFromInline = true
        // MediaPlayer keeps the lock screen up to date (title, artwork, queue).
        controller.updatesNowPlayingInfoCenter = false
        controller.videoGravity = .resizeAspect
        if let overlay = controller.contentOverlayView {
            let host = UIHostingController(rootView: SubtitleOverlay().environment(media))
            host.view.backgroundColor = .clear
            host.view.isUserInteractionEnabled = false
            host.view.frame = overlay.bounds
            host.view.autoresizingMask = [.flexibleWidth, .flexibleHeight]
            controller.addChild(host)
            overlay.addSubview(host.view)
            host.didMove(toParent: controller)
        }
        return controller
    }

    func updateUIViewController(_ controller: AVPlayerViewController, context: Context) {}
}

private struct SubtitleOverlay: View {
    @Environment(MediaPlayer.self) private var media

    var body: some View {
        VStack {
            Spacer()
            if media.cueText == nil, let status = media.subtitleStatus {
                Label(status, systemImage: "captions.bubble")
                    .font(.footnote.weight(.medium))
                    .foregroundStyle(.white.opacity(0.85))
                    .padding(.horizontal, 10)
                    .padding(.vertical, 4)
                    .background(.black.opacity(0.45), in: Capsule())
                    .padding(.bottom, 64)
            }
            if let text = media.cueText {
                Text(text)
                    .font(.system(size: 20, weight: .semibold))
                    .multilineTextAlignment(.center)
                    .foregroundStyle(.white)
                    .shadow(color: .black, radius: 2)
                    .padding(.horizontal, 10)
                    .padding(.vertical, 4)
                    .background(.black.opacity(0.45), in: RoundedRectangle(cornerRadius: 6))
                    .padding(.bottom, 64)
                    .padding(.horizontal, 24)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

private struct AudioPlayerScreen: View {
    @Environment(MediaPlayer.self) private var media
    @Environment(\.dismiss) private var dismiss
    @State private var scrubbing: Double?

    var body: some View {
        VStack(spacing: 24) {
            HStack {
                Button { dismiss() } label: { Image(systemName: "chevron.down").font(.title3.weight(.semibold)) }
                    .accessibilityLabel("Close")
                    .accessibilityIdentifier("closePlayer")
                Spacer()
                RoutePicker().frame(width: 32, height: 32)
            }
            Spacer(minLength: 0)
            if let item = media.current {
                Poster(path: item.posterUrl, kind: item.kind)
                    .aspectRatio(1, contentMode: .fit)
                    .clipShape(RoundedRectangle(cornerRadius: LucinaTokens.Radius.tile))
                    .shadow(radius: 12, y: 6)
                    .frame(maxWidth: 320)
                VStack(spacing: 4) {
                    Text(item.title).font(LucinaTokens.Fonts.title3).foregroundStyle(Theme.text).multilineTextAlignment(.center)
                    if let album = item.album { Text(album).font(LucinaTokens.Fonts.subheadline).foregroundStyle(Theme.textSecondary) }
                }
            }
            if let error = media.error {
                Text(error).font(LucinaTokens.Fonts.callout).foregroundStyle(Theme.negative).multilineTextAlignment(.center)
            }
            VStack(spacing: 4) {
                Slider(value: Binding(get: { scrubbing ?? media.position }, set: { scrubbing = $0 }),
                       in: 0...max(media.duration, 1)) { editing in
                    if !editing, let target = scrubbing {
                        media.seek(to: target)
                        scrubbing = nil
                    }
                }
                .accessibilityIdentifier("audioScrubber")
                HStack {
                    Text(MediaFormat.time(scrubbing ?? media.position))
                    Spacer()
                    Text("-" + MediaFormat.time(max(0, media.duration - (scrubbing ?? media.position))))
                }
                .font(LucinaTokens.Fonts.caption2.monospacedDigit())
                .foregroundStyle(Theme.textSecondary)
            }
            HStack(spacing: 28) {
                Button { media.previous() } label: { Image(systemName: "backward.end.fill") }
                    .accessibilityLabel("Previous")
                Button { media.skip(by: -15) } label: { Image(systemName: "gobackward.15") }
                    .accessibilityLabel("Back 15 seconds")
                Button { media.togglePlay() } label: {
                    Image(systemName: media.isPlaying ? "pause.circle.fill" : "play.circle.fill").font(.system(size: 64))
                }
                .accessibilityLabel(media.isPlaying ? "Pause" : "Play")
                .accessibilityIdentifier("playPause")
                Button { media.skip(by: 15) } label: { Image(systemName: "goforward.15") }
                    .accessibilityLabel("Forward 15 seconds")
                Button { media.next() } label: { Image(systemName: "forward.end.fill") }
                    .disabled(!media.hasNext)
                    .accessibilityLabel("Next")
            }
            .font(.title2)
            .overlay { if media.loading { ProgressView().offset(y: 52) } }
            Menu {
                ForEach([Float(0.75), 1, 1.25, 1.5, 2], id: \.self) { r in
                    Button(r == 1 ? "1×" : "\(r.formatted())×") { media.setRate(r) }
                }
            } label: {
                Text(media.rate == 1 ? "1×" : "\(media.rate.formatted())×")
                    .font(LucinaTokens.Fonts.subheadline.weight(.semibold).monospacedDigit())
                    .foregroundStyle(Theme.text)
                    .padding(.horizontal, 14)
                    .frame(height: 34)
                    .background(Theme.field, in: Capsule())
            }
            Spacer(minLength: 0)
        }
        .padding(24)
        .background(Theme.page.ignoresSafeArea())
        .tint(Theme.accent)
    }
}

private struct RoutePicker: UIViewRepresentable {
    func makeUIView(context: Context) -> AVRoutePickerView {
        let view = AVRoutePickerView()
        view.prioritizesVideoDevices = false
        return view
    }

    func updateUIView(_ uiView: AVRoutePickerView, context: Context) {}
}
