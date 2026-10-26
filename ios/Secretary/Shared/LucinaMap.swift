import MapKit
import SwiftUI

// The one map of the app (MapKit, no keys, no third party): pins with titles, the owner's
// position, the camera fitted to the pins, a pin focused on request. Used by the deals map and
// the place screen; the chat cards use a snapshot instead (PlaceCard.swift).

/// A pin: a coloured pill with a short label (the deals markers), else a dot; home is a house.
struct MapPin: Identifiable, Equatable {
    enum Tint: Equatable, Sendable {
        case accent, clearance, neutral, home

        func color(selected: Bool) -> Color {
            if selected { return Theme.accentFill }
            switch self {
            case .accent: return Theme.accent
            case .clearance: return Theme.warning
            case .neutral: return Color(uiColor: .systemGray)
            case .home: return Theme.accentFill
            }
        }
    }

    let id: String
    let coordinate: CLLocationCoordinate2D
    let title: String
    var subtitle: String?
    var label: String?
    var tint: Tint = .accent

    init(id: String, coordinate: CLLocationCoordinate2D, title: String, subtitle: String? = nil, label: String? = nil, tint: Tint = .accent) {
        self.id = id
        self.coordinate = coordinate
        self.title = title
        self.subtitle = subtitle
        self.label = label
        self.tint = tint
    }

    static func == (a: MapPin, b: MapPin) -> Bool {
        a.id == b.id && a.coordinate.latitude == b.coordinate.latitude && a.coordinate.longitude == b.coordinate.longitude
            && a.title == b.title && a.subtitle == b.subtitle && a.label == b.label && a.tint == b.tint
    }
}

/// A request to move the camera to one pin; a new tick repeats it for the same pin.
struct MapFocus: Equatable {
    let id: String
    let tick: Int
}

struct LucinaMap: View {
    let pins: [MapPin]
    var selectedId: Binding<String?>?
    var showsUserLocation = false
    var focus: MapFocus?
    /// False: a still picture of the map (no gestures, no controls).
    var interactive = true
    /// Fitted together with the pins (the owner's position on the place screen).
    var alsoFit: CLLocationCoordinate2D?
    /// The camera's centre after each move.
    var onCameraChange: ((CLLocationCoordinate2D) -> Void)?

    /// Selected route as latitude/longitude pairs; fitted together with the pins.
    var polyline: [[Double]] = []

    @State private var position: MapCameraPosition
    @State private var fitKey: FitKey
    @State private var cameraDistance: Double?

    /// One pin: the camera this far above it (about zoom 15).
    static let singlePinDistance: Double = 1500

    init(pins: [MapPin], selectedId: Binding<String?>? = nil, showsUserLocation: Bool = false, focus: MapFocus? = nil,
         interactive: Bool = true, polyline: [[Double]] = [], alsoFit: CLLocationCoordinate2D? = nil,
         onCameraChange: ((CLLocationCoordinate2D) -> Void)? = nil) {
        self.pins = pins
        self.polyline = polyline
        self.selectedId = selectedId
        self.showsUserLocation = showsUserLocation
        self.focus = focus
        self.interactive = interactive
        self.alsoFit = alsoFit
        self.onCameraChange = onCameraChange
        _position = State(initialValue: Self.fit(pins, also: alsoFit, polyline: polyline))
        _fitKey = State(initialValue: FitKey(pins: pins, also: alsoFit, polyline: polyline))
    }

    /// The fit changes only when the set of pins (or the extra point) does, not on a new label or selection.
    private struct FitKey: Equatable {
        let ids: [String]
        let also: Bool
        let polyline: [[Double]]

        init(pins: [MapPin], also: CLLocationCoordinate2D?, polyline: [[Double]]) {
            ids = pins.map(\.id).sorted()
            self.also = also != nil
            self.polyline = polyline
        }
    }

    private var selected: String? { selectedId?.wrappedValue }

    /// The selected pin last, so it draws on top.
    private var ordered: [MapPin] {
        guard let selected else { return pins }
        return pins.filter { $0.id != selected } + pins.filter { $0.id == selected }
    }

    var body: some View {
        Map(position: $position, interactionModes: interactive ? .all : []) {
            if polyline.count > 1 {
                MapPolyline(coordinates: Self.lineCoordinates(polyline))
                    .stroke(Theme.accent, lineWidth: 4)
            }
            ForEach(ordered) { pin in
                Annotation(pin.title, coordinate: pin.coordinate, anchor: pin.label == nil ? .center : .bottom) {
                    LucinaMapPinView(pin: pin, selected: pin.id == selected)
                        .onTapGesture { select(pin) }
                        .accessibilityElement(children: .ignore)
                        .accessibilityLabel(Text([pin.title, pin.subtitle].compactMap { $0 }.joined(separator: ", ")))
                        .accessibilityAddTraits(pin.tint == .home || selectedId == nil ? [] : .isButton)
                }
            }
            if showsUserLocation && PlaceLocation.shared.isAuthorized {
                UserAnnotation()
            }
        }
        .mapStyle(.standard(pointsOfInterest: .excludingAll))
        .mapControls {
            if interactive {
                MapUserLocationButton()
                MapScaleView()
            }
        }
        .allowsHitTesting(interactive)
        .onMapCameraChange(frequency: .onEnd) { context in
            cameraDistance = context.camera.distance
            onCameraChange?(context.camera.centerCoordinate)
        }
        .onChange(of: FitKey(pins: pins, also: alsoFit, polyline: polyline)) { _, key in
            guard key != fitKey else { return }
            fitKey = key
            withAnimation(.easeInOut(duration: 0.35)) { position = Self.fit(pins, also: alsoFit, polyline: polyline) }
        }
        .onChange(of: focus) { _, focus in
            guard let focus, let pin = pins.first(where: { $0.id == focus.id }) else { return }
            let distance = min(cameraDistance ?? Self.singlePinDistance, Self.singlePinDistance)
            withAnimation(.easeInOut(duration: 0.35)) {
                position = .camera(MapCamera(centerCoordinate: pin.coordinate, distance: distance))
            }
        }
    }

    private func select(_ pin: MapPin) {
        guard pin.tint != .home, let selectedId else { return }
        selectedId.wrappedValue = pin.id
    }

    private static func lineCoordinates(_ line: [[Double]]) -> [CLLocationCoordinate2D] {
        line.compactMap { pair in
            guard pair.count == 2 else { return nil }
            return Place.validCoordinate(pair[0], pair[1])
        }
    }

    // MARK: Fit

    /// Every pin (and the extra point) in view with ~20 % padding; one point: the camera 1.5 km above it.
    static func fit(_ pins: [MapPin], also: CLLocationCoordinate2D? = nil, polyline: [[Double]] = []) -> MapCameraPosition {
        guard let rect = fitRect(pins, also: also, polyline: polyline) else {
            guard let only = pins.first?.coordinate ?? also else { return .automatic }
            return .camera(MapCamera(centerCoordinate: only, distance: singlePinDistance))
        }
        return .rect(rect)
    }

    /// Where `fit` puts the camera's centre (the self-test compares it with the real camera).
    static func fitCenter(_ pins: [MapPin], also: CLLocationCoordinate2D? = nil, polyline: [[Double]] = []) -> CLLocationCoordinate2D? {
        guard let rect = fitRect(pins, also: also, polyline: polyline) else { return pins.first?.coordinate ?? also }
        return MKMapPoint(x: rect.midX, y: rect.midY).coordinate
    }

    /// The padded rectangle around two or more points (at least ~1 km across); nil for fewer.
    private static func fitRect(_ pins: [MapPin], also: CLLocationCoordinate2D?, polyline: [[Double]]) -> MKMapRect? {
        let coordinates = pins.map(\.coordinate) + (also.map { [$0] } ?? []) + lineCoordinates(polyline)
        guard coordinates.count > 1 else { return nil }
        var rect = MKMapRect.null
        for coordinate in coordinates {
            let point = MKMapPoint(coordinate)
            rect = rect.union(MKMapRect(x: point.x, y: point.y, width: 0, height: 0))
        }
        let minSide = 1000 * MKMapPointsPerMeterAtLatitude(coordinates[0].latitude)
        if rect.width < minSide { rect = rect.insetBy(dx: (rect.width - minSide) / 2, dy: 0) }
        if rect.height < minSide { rect = rect.insetBy(dx: 0, dy: (rect.height - minSide) / 2) }
        return rect.insetBy(dx: -rect.width * 0.2, dy: -rect.height * 0.2)
    }
}

/// A pin's look: a pill with the label and a pointer, a dot, or the house.
struct LucinaMapPinView: View {
    let pin: MapPin
    let selected: Bool

    var body: some View {
        let fill = pin.tint.color(selected: selected)
        if pin.tint == .home {
            Image(systemName: "house.fill")
                .font(.system(size: 14, weight: .bold))
                .foregroundStyle(.white)
                .frame(width: 30, height: 30)
                .background(fill, in: Circle())
                .overlay(Circle().strokeBorder(.white, lineWidth: 2))
                .shadow(color: .black.opacity(0.25), radius: 2, y: 1)
        } else if let label = pin.label {
            let height: CGFloat = selected ? 30 : 26
            VStack(spacing: -1) {
                Text(verbatim: label)
                    .font(.system(size: selected ? 15 : 13, weight: .bold).monospacedDigit())
                    .foregroundStyle(.white)
                    .lineLimit(1)
                    .padding(.horizontal, 8)
                    .frame(minWidth: height, minHeight: height)
                    .background(fill, in: Capsule())
                    .overlay(Capsule().strokeBorder(.white, lineWidth: 2))
                PinPointer()
                    .fill(fill)
                    .frame(width: 12, height: 7)
            }
            .shadow(color: .black.opacity(0.25), radius: 2, y: 1)
        } else {
            Circle()
                .fill(fill)
                .frame(width: selected ? 22 : 18, height: selected ? 22 : 18)
                .overlay(Circle().strokeBorder(.white, lineWidth: 2))
                .shadow(color: .black.opacity(0.25), radius: 2, y: 1)
        }
    }
}

/// The small triangle under a pill pin.
private struct PinPointer: Shape {
    func path(in rect: CGRect) -> Path {
        var path = Path()
        path.move(to: CGPoint(x: rect.minX, y: rect.minY))
        path.addLine(to: CGPoint(x: rect.midX, y: rect.maxY))
        path.addLine(to: CGPoint(x: rect.maxX, y: rect.minY))
        path.closeSubpath()
        return path
    }
}
