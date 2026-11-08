import Security
import XCTest
@testable import Secretary

final class KeychainTests: XCTestCase {
    private let store = KeychainStore(service: "tech.getsecretary.app.tests", account: "round-trip-\(UUID().uuidString)")

    override func tearDown() {
        try? store.delete()
    }

    func testRoundTripOverwriteAndDelete() throws {
        XCTAssertNil(try store.read())
        try store.save("sec_first")
        XCTAssertEqual(try store.read(), "sec_first")
        try store.save("sec_second")
        XCTAssertEqual(try store.read(), "sec_second")
        XCTAssertEqual(try store.accessibility(), kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly as String)
        try store.delete()
        XCTAssertNil(try store.read())
        // Deleting twice is fine.
        try store.delete()
    }

    func testTheTokenIsNotInUserDefaults() throws {
        try store.save("sec_secret_value")
        let everything = UserDefaults.standard.dictionaryRepresentation().values.compactMap { $0 as? String }
        XCTAssertFalse(everything.contains("sec_secret_value"))
    }
}
