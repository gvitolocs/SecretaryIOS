import SwiftUI
import UIKit

struct NClientReading: Identifiable {
    let id = UUID()
    let detail: NClientDetail
    let page: Int
}

struct NClientReader: View {
    let detail: NClientDetail
    let store: NClientStore
    let loader: NClientImageLoader
    @Environment(\.dismiss) private var dismiss
    @Environment(\.scenePhase) private var scenePhase
    @State private var page: Int
    @State private var bars = true

    init(detail: NClientDetail, page: Int, store: NClientStore, loader: NClientImageLoader) {
        self.detail = detail; self.store = store; self.loader = loader
        _page = State(initialValue: min(max(0, page), max(0, detail.pages.count - 1)))
    }

    var body: some View {
        ZStack {
            Color.black.ignoresSafeArea()
            TabView(selection: $page) {
                ForEach(Array(detail.pages.enumerated()), id: \.offset) { offset, item in
                    NClientReaderPage(url: store.api()?.imageURL(item.url), loader: loader, active: offset == page, tap: { bars.toggle() })
                        .tag(offset)
                }
            }
            .tabViewStyle(.page(indexDisplayMode: .never))
            .accessibilityIdentifier("nclient-reader-pages")
            if bars {
                VStack {
                    HStack {
                        Button("Close", systemImage: "xmark") { close() }
                            .accessibilityIdentifier("nclient-reader-close")
                        Spacer()
                        Text("\(page + 1) / \(detail.pages.count)").monospacedDigit()
                    }
                    .padding().background(.black.opacity(0.85))
                    Spacer()
                    if let error = store.error { Text(verbatim: error).font(.caption) }
                    if let error = store.progressError {
                        Text(verbatim: error).font(.caption)
                        Button("Try again") { flush() }
                    }
                    ScrollView(.horizontal) {
                        HStack(alignment: .top, spacing: 20) {
                            Button((store.details[detail.id]?.favorite ?? detail.favorite) ? LocalizedStringKey("Unfavourite") : LocalizedStringKey("Favourite"), systemImage: "heart") {
                                Task { await store.toggleFavorite(detail.id, current: store.details[detail.id]?.favorite ?? detail.favorite) }
                            }.disabled(store.favoriteBusy.contains(detail.id))
                            NClientSaveControl(id: detail.id, store: store)
                        }.padding()
                    }.background(.black.opacity(0.85))
                    HStack {
                        Button("Previous", systemImage: "chevron.left") { page = max(0, page - 1) }.disabled(page == 0)
                        Spacer()
                        Button("Next", systemImage: "chevron.right") { page = min(detail.pages.count - 1, page + 1) }
                            .disabled(page >= detail.pages.count - 1)
                    }
                    .padding().background(.black.opacity(0.85))
                    if detail.pages.count > 1 {
                        Slider(value: Binding(get: { Double(page) }, set: { page = Int($0.rounded()) }), in: 0...Double(detail.pages.count - 1), step: 1) {
                            Text("Page")
                        }.padding(.horizontal).accessibilityIdentifier("nclient-reader-position")
                    }
                }
            }
        }
        .foregroundStyle(.white)
        .persistentSystemOverlays(bars ? .visible : .hidden)
        .task(id: page) {
            do { try await Task.sleep(for: .seconds(1)); try Task.checkCancellation() }
            catch { return }
            await store.saveProgress(detail.id, page: page, pages: detail.pages.count)
        }
        .task(id: page) {
            guard detail.pages.indices.contains(page), let api = store.api(),
                  let current = api.imageURL(detail.pages[page].url) else { return }
            // Wait for the visible page before queueing work on the service's spaced fetcher.
            guard await loader.image(current) != nil, !Task.isCancelled else { return }
            for next in (page + 1)..<min(page + 3, detail.pages.count) {
                guard !Task.isCancelled else { return }
                if let url = api.imageURL(detail.pages[next].url) { _ = await loader.image(url) }
            }
        }
        .onChange(of: scenePhase) { _, phase in
            if phase != .active { flush() }
        }
        .onDisappear { flush() }
        .modifier(NClientPrivacy())
    }

    private func flush() {
        let current = page
        Task { await store.saveProgress(detail.id, page: current, pages: detail.pages.count) }
    }
    private func close() { flush(); dismiss() }
}

private struct NClientReaderPage: View {
    let url: URL?
    let loader: NClientImageLoader
    let active: Bool
    let tap: () -> Void
    @State private var image: UIImage?
    @State private var failed = false
    @State private var copied = false
    @State private var retry = 0
    var body: some View {
        Group {
            if let image {
                NClientZoomPage(image: image, tap: tap)
                    .contextMenu {
                        Button("Copy image", systemImage: "doc.on.doc") {
                            UIPasteboard.general.image = image; copied.toggle()
                        }
                        ShareLink(item: Image(uiImage: image), preview: SharePreview(String(localized: "Page"), image: Image(uiImage: image))) {
                            Label("Share image", systemImage: "square.and.arrow.up")
                        }
                    }
            } else if failed {
                ContentUnavailableView {
                    Label("Couldn't load page", systemImage: "photo")
                } actions: { Button("Try again") { retry += 1 } }
            } else { ProgressView().tint(.white) }
        }
        .sensoryFeedback(.success, trigger: copied)
        .task(id: "\(url?.absoluteString ?? "")-\(retry)-\(active)") {
            guard active else { return }
            failed = false
            guard let url else { failed = true; return }
            image = await loader.image(url); failed = image == nil
        }
    }
}
