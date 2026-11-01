import MapKit
import SwiftUI

/// The location card's map: a MapKit snapshot around the place with a pin in the middle.
struct CalendarMapThumb: View {
    let coordinate: CLLocationCoordinate2D
    @Environment(\.colorScheme) private var scheme
    @State private var image: UIImage?

    private static let size = CGSize(width: 116, height: 80)

    var body: some View {
        ZStack {
            if let image {
                Image(uiImage: image).resizable().scaledToFill()
            } else {
                Rectangle().fill(.quaternary)
            }
            Image(systemName: "mappin.circle.fill")
                .font(.title2)
                .symbolRenderingMode(.palette)
                .foregroundStyle(.white, .red)
                .shadow(radius: 1)
        }
        .frame(width: Self.size.width, height: Self.size.height)
        .clipShape(RoundedRectangle(cornerRadius: 12))
        .accessibilityHidden(true)
        .task(id: "\(coordinate.latitude),\(coordinate.longitude),\(scheme == .dark)") { image = await snapshot() }
    }

    private func snapshot() async -> UIImage? {
        let options = MKMapSnapshotter.Options()
        options.region = MKCoordinateRegion(center: coordinate, latitudinalMeters: 700, longitudinalMeters: 700)
        options.size = Self.size
        options.traitCollection = UITraitCollection(userInterfaceStyle: scheme == .dark ? .dark : .light)
        options.pointOfInterestFilter = .excludingAll
        return try? await MKMapSnapshotter(options: options).start().image
    }
}

/// The hours around an event with the events in them (CalendarMath.dayStrip): this one solid, the others tinted.
struct CalendarDayStripView: View {
    let strip: CalendarMath.DayStrip
    private let hourHeight: CGFloat = 40
    private let labelWidth: CGFloat = 46

    var body: some View {
        let total = max(1, strip.end.timeIntervalSince(strip.start))
        let height = CGFloat(total / 3600) * hourHeight
        GeometryReader { geo in
            let lane = max(40, geo.size.width - labelWidth - 6)
            ZStack(alignment: .topLeading) {
                ForEach(strip.hours, id: \.self) { hour in
                    let y = CGFloat(hour.timeIntervalSince(strip.start) / total) * height
                    HStack(spacing: 6) {
                        Text(CalendarMath.format(hour, template: "HHmm"))
                            .font(.caption2.monospacedDigit()).foregroundStyle(.secondary)
                            .frame(width: labelWidth, alignment: .trailing)
                        Rectangle().fill(Color.secondary.opacity(0.3)).frame(height: 0.5)
                    }
                    .frame(height: 12)
                    .offset(y: y - 6)
                }
                ForEach(strip.items) { item in
                    let y = CGFloat(item.start.timeIntervalSince(strip.start) / total) * height
                    let h = max(18, CGFloat(item.end.timeIntervalSince(item.start) / total) * height)
                    let width = lane / CGFloat(item.columns)
                    let tint = CalendarMath.color(name: item.event.calendarName, source: item.event.source)
                    RoundedRectangle(cornerRadius: 6)
                        .fill(tint.opacity(item.current ? 0.9 : 0.22))
                        .overlay(alignment: .leading) { if !item.current { Rectangle().fill(tint).frame(width: 3) } }
                        .overlay(alignment: .topLeading) {
                            Text(item.event.title)
                                .font(.caption.weight(item.current ? .semibold : .regular))
                                .foregroundStyle(item.current ? Color.white : Color.primary)
                                .lineLimit(h > 34 ? 2 : 1)
                                .padding(.horizontal, 6).padding(.vertical, 3)
                        }
                        .clipShape(RoundedRectangle(cornerRadius: 6))
                        .frame(width: max(8, width - 3), height: h)
                        .offset(x: labelWidth + 6 + width * CGFloat(item.column), y: y)
                        .accessibilityElement(children: .ignore)
                        .accessibilityLabel(Text(item.event.title + ", " + CalendarMath.format(item.event.startDate, template: "HHmm")
                                                 + "–" + CalendarMath.format(item.event.endDate, template: "HHmm")))
                }
            }
        }
        .frame(height: height)
        .padding(.vertical, 6)
    }
}
