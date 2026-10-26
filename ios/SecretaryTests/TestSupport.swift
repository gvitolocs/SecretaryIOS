import Foundation
@testable import Secretary

/// Answers URLSession requests from a closure, so APIClient can be tested without a network.
final class StubURLProtocol: URLProtocol, @unchecked Sendable {
    struct Reply {
        var status: Int = 200
        var body: String = "{}"
        var headers: [String: String] = ["Content-Type": "application/json"]
        var error: URLError?
    }

    nonisolated(unsafe) static var handler: ((URLRequest) -> Reply)?
    nonisolated(unsafe) static var requests: [URLRequest] = []

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        var recorded = request
        if recorded.httpBody == nil, let stream = request.httpBodyStream {
            stream.open()
            var data = Data()
            var buffer = [UInt8](repeating: 0, count: 4096)
            while stream.hasBytesAvailable {
                let count = stream.read(&buffer, maxLength: buffer.count)
                if count <= 0 { break }
                data.append(buffer, count: count)
            }
            stream.close()
            recorded.httpBody = data
        }
        Self.requests.append(recorded)
        let reply = Self.handler?(recorded) ?? Reply()
        if let error = reply.error {
            client?.urlProtocol(self, didFailWithError: error)
            return
        }
        let response = HTTPURLResponse(url: request.url!, statusCode: reply.status, httpVersion: "HTTP/1.1", headerFields: reply.headers)!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: Data(reply.body.utf8))
        client?.urlProtocolDidFinishLoading(self)
    }

    override func stopLoading() {}

    static func makeClient(key: String? = "sec_test") -> APIClient {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [StubURLProtocol.self]
        return APIClient(baseURL: URL(string: "https://example.test")!, accessKey: key, session: URLSession(configuration: configuration))
    }
}

enum Fixtures {
    static func decode<T: Decodable>(_ type: T.Type, _ json: String) throws -> T {
        try JSONDecoder().decode(T.self, from: Data(json.utf8))
    }

    /// A gym view shaped like docs/server-api.md (newer engine fields included).
    static func gymView(revision: Int = 3, active: Bool = false) -> String {
        let activeJSON = active ? """
        {"id":"w-active","title":"Workout","notes":"","started_ms":1792600000000,"finished_ms":null,
         "exercises":[{"id":"we-1","exercise_id":"bench-press","notes":"","sets":[
           {"id":"s-1","kind":"normal","weight_grams":60000,"reps":8,"seconds":0,"distance_meters":0,"completed":false}]}]}
        """ : "null"
        return """
        {
          "state": {
            "schema_version": 1, "revision": \(revision),
            "settings": {"weight_unit": "kg", "rest_seconds": 90, "weekly_goal": 3},
            "exercises": [
              {"id":"bench-press","name":"Bench Press","muscle":"Chest","equipment":"Barbell","kind":"weight_reps","custom":false},
              {"id":"plank","name":"Plank","muscle":"Core","equipment":"Bodyweight","kind":"timed","custom":false}
            ],
            "routines": [{"id":"push","name":"Push","exercises":[{"exercise_id":"bench-press","sets":3,"reps":8,"weight_grams":0}]}],
            "active": \(activeJSON),
            "history": [
              {"id":"w-1","title":"Push day","notes":"","started_ms":1792500000000,"finished_ms":1792503600000,
               "exercises":[{"id":"we-0","exercise_id":"bench-press","notes":"","sets":[
                 {"id":"s-0","kind":"normal","weight_grams":60000,"reps":8,"seconds":0,"distance_meters":0,"completed":true}]}]}
            ],
            "measurements": [{"id":"m-1","timestamp_ms":1792500000000,"kind":"waist","value_milli":82000}],
            "rest": null
          },
          "summary": {"workouts":1,"completed_sets":1,"weekly_workouts":1,"weekly_goal":3,"volume":"480 kg",
                      "active_volume":"0 kg","active_duration":"0m 00s","active_elapsed_seconds":0,"active_completed_sets":0},
          "records": [{"exercise_id":"bench-press","name":"Bench Press","weight":"60 kg","reps":8,"volume":"480 kg",
                       "seconds":0,"distance_meters":0,"estimated_1rm":"76 kg","estimated_1rm_grams":76000}],
          "chart": [{"id":"w-1","label":"20 Oct","volume":480,"volume_grams":480000}],
          "exercise_charts": {"bench-press":[{"id":"w-1","label":"20 Oct","volume":480,"volume_grams":480000}]},
          "rest_remaining": 0,
          "weight_labels": {"s-0":"60","s-1":"60"},
          "previous_labels": {"we-1":"60 kg × 8"},
          "measurement_labels": {"m-1":"82 cm"},
          "workout_labels": {"w-1":{"sets":1,"volume":"480 kg","duration":"1h 00m"}}
        }
        """
    }
}
