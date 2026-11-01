import XCTest
@testable import Secretary

final class PluginRouteTests: XCTestCase {
    func testInstagramProfileRoutes() {
        XCTAssertEqual(PluginRoute.instagramUser("/instagram/natgeo"), "natgeo")
        XCTAssertEqual(PluginRoute.instagramUser("instagram/_ter.restre?x=1"), "_ter.restre")
        XCTAssertEqual(PluginRoute.instagramUser("/instagram/NatGeo"), "natgeo")
        XCTAssertNil(PluginRoute.instagramUser("/instagram"))
        XCTAssertNil(PluginRoute.instagramUser("/instagram/not%20a%20name"))
    }

    func testPixivWorkRoutes() {
        XCTAssertEqual(PluginRoute.pixivWork("/pixiv/123456"), "123456")
        XCTAssertEqual(PluginRoute.pixivWork("/pixiv/123456?adult=1"), "123456")
        XCTAssertNil(PluginRoute.pixivWork("/pixiv"))
        XCTAssertNil(PluginRoute.pixivWork("/pixiv/12ab"))
        XCTAssertNil(PluginRoute.pixivWork("/pixiv/1234567890123"))
    }

    func testSegmentIgnoresTheQuery() {
        XCTAssertEqual(PluginRoute.segment("/calendar?event=x", 0), "calendar")
        XCTAssertNil(PluginRoute.segment("/calendar?event=x", 1))
    }
}
