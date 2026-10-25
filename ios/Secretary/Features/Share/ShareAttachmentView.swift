import SwiftUI
import UIKit

struct ShareAttachmentView: View {
    @Environment(AppModel.self) private var app
    let attachment: TimelineAttachment
    @State private var image: UIImage?
    @State private var largeImage: UIImage?
    @State private var showViewer = false

    private var isVisual: Bool { ["image", "photo", "video"].contains(attachment.kind) && attachment.mediaId != nil }
    private var link: URL? { attachment.url.flatMap(ShareItemReader.webURL) }
    var body: some View {
        Group {
            if isVisual {
                Button { showViewer = true } label: {
                    ZStack {
                        if let image { Image(uiImage: image).resizable().scaledToFit() }
                        else { Rectangle().fill(Theme.field).frame(height: 140).overlay(Image(systemName: TimelineAttachment.symbol(for: attachment.kind))) }
                        if attachment.kind == "video" { Image(systemName: "play.circle.fill").font(.largeTitle).foregroundStyle(.white).shadow(radius: 3) }
                    }.frame(maxWidth: 220, maxHeight: 220).clipShape(RoundedRectangle(cornerRadius: 14))
                }.buttonStyle(.plain).accessibilityLabel(Text(attachment.name ?? String(localized: "Shared media")))
                    .task(id: attachment.mediaId) { image = await load(size: 256) }
                    .fullScreenCover(isPresented: $showViewer) {
                        ZStack(alignment: .topTrailing) {
                            Color.black.ignoresSafeArea()
                            if let image = largeImage ?? image { Image(uiImage: image).resizable().scaledToFit().frame(maxWidth: .infinity, maxHeight: .infinity) }
                            Button("Close") { showViewer = false }.padding()
                        }.task { largeImage = await load(size: 1024) }
                    }
            } else if attachment.kind == "link", let link {
                Link(destination: link) { capsule(attachment.name ?? link.host ?? link.absoluteString) }
            } else { capsule(attachment.name ?? attachment.kind.capitalized) }
        }
    }

    private func capsule(_ title: String) -> some View {
        Label(title, systemImage: TimelineAttachment.symbol(for: attachment.kind))
            .font(.caption).padding(.horizontal, 10).padding(.vertical, 6).background(Theme.field, in: Capsule())
    }

    private func load(size: Int) async -> UIImage? {
        guard let id = attachment.mediaId, !id.isEmpty, let token = app.client.accessKey else { return nil }
        for endpoint in [app.client.baseURL] + app.client.endpoints.filter({ $0 != app.client.baseURL }) {
            let url = endpoint.appendingPathComponent("v1/media/items").appendingPathComponent(id).appendingPathComponent("thumb")
            guard var components = URLComponents(url: url, resolvingAgainstBaseURL: false) else { continue }
            components.queryItems = [URLQueryItem(name: "size", value: String(size))]
            guard let url = components.url else { continue }
            var request = URLRequest(url: url)
            request.setValue("Bearer " + token, forHTTPHeaderField: "Authorization")
            do {
                let (data, response) = try await URLSession.shared.data(for: request)
                if let status = (response as? HTTPURLResponse)?.statusCode, (200..<300).contains(status), let image = UIImage(data: data) { return image }
            } catch { if Task.isCancelled { return nil } }
        }
        return nil
    }
}
