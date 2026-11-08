import SwiftUI
import QuickLook

/// The horizontal preview row above the composer and in the share sheet: one card per file with its
/// thumbnail or icon, name and size, a duration badge on videos and audio, a remove button, and tap to
/// preview with Quick Look. Built on LucinaTokens only, so the share extension can use it too.
struct AttachmentTray: View {
    @Binding var attachments: [PendingAttachment]
    /// Deletes the removed file from the pending folder (the tray owns its copies).
    var deletesFiles = true
    @State private var preview: URL?

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                ForEach(attachments) { attachment in
                    AttachmentTrayCard(attachment: attachment,
                                       onPreview: { preview = attachment.url },
                                       onRemove: { remove(attachment) })
                }
            }
            .padding(.horizontal, 2)
            .padding(.vertical, 4)
        }
        .quickLookPreview($preview, in: attachments.map(\.url))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("attachmentTray")
    }

    private func remove(_ attachment: PendingAttachment) {
        attachments.removeAll { $0.id == attachment.id }
        if deletesFiles { try? FileManager.default.removeItem(at: attachment.url) }
    }
}

struct AttachmentTrayCard: View {
    let attachment: PendingAttachment
    var onPreview: () -> Void
    var onRemove: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Button(action: onPreview) {
                ZStack(alignment: .bottomLeading) {
                    thumbnail
                        .frame(width: 76, height: 76)
                        .clipShape(RoundedRectangle(cornerRadius: LucinaTokens.Radius.tile, style: .continuous))
                    if let ms = attachment.durationMs, attachment.kind == .video || attachment.kind == .audio {
                        Text(Self.duration(ms))
                            .font(.caption2.monospacedDigit().weight(.semibold))
                            .foregroundStyle(.white)
                            .padding(.horizontal, 5)
                            .padding(.vertical, 2)
                            .background(.black.opacity(0.55), in: Capsule())
                            .padding(5)
                    }
                    if attachment.isTranscribing {
                        ProgressView().controlSize(.small).tint(.white)
                            .padding(5)
                            .background(.black.opacity(0.45), in: Circle())
                            .padding(5)
                            .frame(maxWidth: .infinity, alignment: .trailing)
                    }
                }
            }
            .buttonStyle(.plain)
            .overlay(alignment: .topTrailing) {
                Button(action: onRemove) {
                    Image(systemName: "xmark")
                        .font(.caption2.weight(.bold))
                        .foregroundStyle(.white)
                        .frame(width: 22, height: 22)
                        .background(.black.opacity(0.6), in: Circle())
                        .overlay(Circle().strokeBorder(.white.opacity(0.6), lineWidth: 1))
                }
                .buttonStyle(.plain)
                .offset(x: 6, y: -6)
                .accessibilityLabel(Text("Remove \(attachment.name)"))
            }
            Text(attachment.name)
                .font(.caption2)
                .foregroundStyle(LucinaTokens.Colors.text)
                .lineLimit(1)
                .truncationMode(.middle)
            Text(attachment.sizeText)
                .font(.caption2)
                .foregroundStyle(LucinaTokens.Colors.textSecondary)
                .lineLimit(1)
        }
        .frame(width: 80)
        .accessibilityElement(children: .contain)
        .accessibilityLabel(Text("\(attachment.typeLabel), \(attachment.name), \(attachment.sizeText)"))
    }

    @ViewBuilder private var thumbnail: some View {
        if let image = attachment.thumbnail {
            Image(uiImage: image).resizable().scaledToFill()
        } else {
            ZStack {
                LucinaTokens.Colors.cardRaised
                VStack(spacing: 4) {
                    Image(systemName: attachment.kind.symbol).font(.title2).foregroundStyle(LucinaTokens.Colors.accent)
                    Text(attachment.typeLabel).font(.caption2.weight(.semibold)).foregroundStyle(LucinaTokens.Colors.textSecondary)
                        .lineLimit(1)
                }
            }
        }
    }

    static func duration(_ ms: Int) -> String {
        let s = max(0, ms / 1000)
        return s >= 3600 ? String(format: "%d:%02d:%02d", s / 3600, s / 60 % 60, s % 60) : String(format: "%d:%02d", s / 60, s % 60)
    }
}
