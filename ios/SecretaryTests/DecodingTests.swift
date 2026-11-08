import XCTest
@testable import Secretary

final class DecodingTests: XCTestCase {
    func testHealthOnlineAndOffline() throws {
        let online = try Fixtures.decode(HealthInfo.self, #"{"ok":true,"version":"0.1.0","lucina":{"reachable":true,"latencyMs":12,"apiVersion":1}}"#)
        XCTAssertTrue(online.ok)
        XCTAssertEqual(online.version, "0.1.0")
        XCTAssertEqual(online.lucina, HealthInfo.Lucina(reachable: true, latencyMs: 12, error: nil, apiVersion: 1))

        let down = try Fixtures.decode(HealthInfo.self, #"{"ok":true,"version":"0.1.0","lucina":{"reachable":false,"latencyMs":null,"error":"lucina_unreachable"}}"#)
        XCTAssertEqual(down.lucina?.reachable, false)
        XCTAssertNil(down.lucina?.latencyMs)
        XCTAssertEqual(down.lucina?.error, "lucina_unreachable")
    }

    func testSession() throws {
        XCTAssertEqual(try Fixtures.decode(SessionInfo.self, #"{"authenticated":true,"deviceName":"Giuseppe's iPhone"}"#),
                       SessionInfo(authenticated: true, deviceName: "Giuseppe's iPhone"))
        XCTAssertEqual(try Fixtures.decode(SessionInfo.self, #"{"authenticated":false,"deviceName":null}"#),
                       SessionInfo(authenticated: false, deviceName: nil))
    }

    func testDeviceToken() throws {
        let token = try Fixtures.decode(DeviceToken.self, #"{"token":"sec_abc","deviceId":42,"deviceName":"iPhone"}"#)
        XCTAssertEqual(token.token, "sec_abc")
        XCTAssertEqual(token.deviceId, "42")
        XCTAssertEqual(token.deviceName, "iPhone")
        XCTAssertThrowsError(try Fixtures.decode(DeviceToken.self, #"{"deviceId":"x"}"#))
    }

    func testChatReplyWithAndWithoutJob() throws {
        let reminder = try Fixtures.decode(ChatResponse.self, """
        {"reply":"Reminder set","action":"schedule_reminder","job":null,"sessionId":"secretary_iphone",
         "clientMessageId":"B1E4","reminder":{"id":"r1","text":"x","dueAt":"2026-10-21T16:30:00.000Z"}}
        """)
        XCTAssertEqual(reminder.reply, "Reminder set")
        XCTAssertEqual(reminder.action, "schedule_reminder")
        XCTAssertNil(reminder.job)
        XCTAssertFalse(reminder.duplicate)

        let job = try Fixtures.decode(ChatResponse.self, #"{"reply":"On it.","action":"generate_image","job":{"id":"0c64","status":"queued"},"sessionId":"s","duplicate":true}"#)
        XCTAssertEqual(job.job, JobRef(id: "0c64", status: "queued"))
        XCTAssertTrue(job.duplicate)

        // Unknown actions and a numeric job id are accepted.
        let odd = try Fixtures.decode(ChatResponse.self, #"{"reply":"ok","action":"brand_new_action","job":{"id":7,"status":"running"}}"#)
        XCTAssertEqual(odd.action, "brand_new_action")
        XCTAssertEqual(odd.job?.id, "7")
    }

    func testJobStates() throws {
        let done = try Fixtures.decode(JobInfo.self, """
        {"id":"0c64","action":"generate_image","status":"done","result":"Here it is.","error":null,
         "imageUrl":"https://example.com/i.png","sessionId":"s","clientMessageId":null,"createdAt":"…"}
        """)
        XCTAssertTrue(done.isTerminal)
        XCTAssertEqual(done.result, .string("Here it is."))
        XCTAssertEqual(done.imageUrl, "https://example.com/i.png")

        let error = try Fixtures.decode(JobInfo.self, #"{"id":"1","status":"error","error":"interrupted"}"#)
        XCTAssertTrue(error.isTerminal)
        XCTAssertEqual(error.error, "interrupted")

        let running = try Fixtures.decode(JobInfo.self, #"{"id":"1","status":"running","result":{"rows":2}}"#)
        XCTAssertFalse(running.isTerminal)
        XCTAssertEqual(running.result?.displayText.contains("\"rows\" : 2"), true)
    }

    func testShoppingItemsAreReadDefensively() throws {
        let json = try Fixtures.decode(JSONValue.self, """
        [
          {"id":"3d0b","product":"milk","rawName":"latte","category":"dairy","status":"needed"},
          {"id":"2","product":{"name":"eggs","quantity":"x6"},"status":"PURCHASED"},
          {"id":9,"name":"Bread","quantity":2,"unit":"loaves","checked":true},
          {"rawName":"no id here"},
          {"title":"  "},
          "not an object"
        ]
        """)
        let items = ShoppingItem.list(from: json)
        XCTAssertEqual(items.count, 5)
        XCTAssertEqual(items[0].name, "latte")
        XCTAssertEqual(items[0].category, "dairy")
        XCTAssertFalse(items[0].purchased)
        XCTAssertEqual(items[1].name, "eggs")
        XCTAssertEqual(items[1].quantity, "x6")
        XCTAssertTrue(items[1].purchased)
        XCTAssertEqual(items[2].id, "9")
        XCTAssertEqual(items[2].name, "Bread")
        XCTAssertEqual(items[2].quantity, "2 loaves")
        XCTAssertTrue(items[2].purchased)
        XCTAssertFalse(items[3].hasId)
        XCTAssertEqual(items[3].id, "local-3")
        XCTAssertEqual(items[4].name, "Unnamed item")

        // `{items: [...]}` is accepted too.
        let wrapped = try Fixtures.decode(JSONValue.self, #"{"items":[{"id":"a","rawName":"x","status":"needed"}]}"#)
        XCTAssertEqual(ShoppingItem.list(from: wrapped).map(\.id), ["a"])
    }

    func testShoppingPatchAnswerMergesIntoTheItem() throws {
        let original = ShoppingItem(id: "a", name: "latte", quantity: "1 l", category: "dairy", status: "needed", purchased: false)
        let patch = ShoppingItem(json: try Fixtures.decode(JSONValue.self, #"{"id":"a","product":"latte","status":"purchased"}"#), index: 0)
        let merged = original.merged(with: patch)
        XCTAssertTrue(merged.purchased)
        XCTAssertEqual(merged.status, "purchased")
        XCTAssertEqual(merged.quantity, "1 l")
        XCTAssertEqual(merged.category, "dairy")
    }

    func testGymViewDocShape() throws {
        let view = try Fixtures.decode(GymView.self, Fixtures.gymView(active: true))
        XCTAssertEqual(view.state.revision, 3)
        XCTAssertEqual(view.state.settings.restSeconds, 90)
        XCTAssertEqual(view.state.exercises.map(\.id), ["bench-press", "plank"])
        XCTAssertEqual(view.exercise("plank")?.exerciseKind, .timed)
        XCTAssertEqual(view.state.active?.exercises.first?.sets.first?.weightGrams, 60000)
        XCTAssertNil(view.state.active?.finishedMs)
        XCTAssertEqual(view.state.history.first?.finishedMs, 1792503600000)
        XCTAssertEqual(view.summary.activeElapsedSeconds, 0)
        XCTAssertEqual(view.records.first?.estimated1rm, "76 kg")
        XCTAssertEqual(view.exerciseCharts["bench-press"]?.first?.volume, 480)
        XCTAssertEqual(view.previousLabels["we-1"], "60 kg × 8")
        XCTAssertEqual(view.workoutLabels["w-1"]?.duration, "1h 00m")
        XCTAssertEqual(view.measurementLabels["m-1"], "82 cm")
    }

    func testGymViewOlderEngineWithoutNewerFields() throws {
        let json = """
        {"state":{"schema_version":1,"revision":0,"settings":{"weight_unit":"lb","rest_seconds":60,"weekly_goal":2},
                  "exercises":[],"routines":[],"active":null,"history":[],"measurements":[]},
         "summary":{"workouts":0,"completed_sets":0,"weekly_workouts":0,"weekly_goal":2,"volume":"0 lb",
                    "active_volume":"0 lb","active_duration":"0m 00s","active_completed_sets":0},
         "records":[{"exercise_id":"x","name":"X","weight":"1 lb","reps":1,"volume":"1 lb","seconds":0,"distance_meters":0}],
         "chart":[],"rest_remaining":0,"weight_labels":{},"previous_labels":{},"measurement_labels":{},"workout_labels":{}}
        """
        let view = try Fixtures.decode(GymView.self, json)
        XCTAssertEqual(view.unit, "lb")
        XCTAssertNil(view.summary.activeElapsedSeconds)
        XCTAssertNil(view.records.first?.estimated1rm)
        XCTAssertTrue(view.exerciseCharts.isEmpty)
        XCTAssertNil(view.state.rest)
    }

    func testGymResponses() throws {
        let ok = try Fixtures.decode(GymResponse.self, #"{"ok":true,"replayed":true,"view":\#(Fixtures.gymView(revision: 9))}"#)
        XCTAssertTrue(ok.ok)
        XCTAssertTrue(ok.replayed)
        XCTAssertEqual(ok.viewRevision, 9)

        let conflict = try Fixtures.decode(GymResponse.self, #"{"ok":false,"replayed":false,"code":"conflict","error":"State changed. Refresh and retry the edit."}"#)
        XCTAssertFalse(conflict.ok)
        XCTAssertEqual(conflict.code, "conflict")
        XCTAssertNil(conflict.view)

        let clock = try Fixtures.decode(GymResponse.self, #"{"ok":true,"replayed":false,"clock":{"rest_remaining":42,"active_duration":"12m 05s","active_elapsed_seconds":725}}"#)
        XCTAssertEqual(clock.clock, GymClock(restRemaining: 42, activeDuration: "12m 05s", activeElapsedSeconds: 725))

        let parse = try Fixtures.decode(GymResponse.self, #"{"ok":true,"replayed":false,"weight_grams":80060}"#)
        XCTAssertEqual(parse.weightGrams, 80060)

        // The state route may answer a bare view.
        XCTAssertEqual(try HTTPGymTransport.decodeStateBody(Data(Fixtures.gymView(revision: 4).utf8)).viewRevision, 4)
    }

    func testGymCommandEncoding() throws {
        func json(_ command: GymCommand) throws -> [String: Any] {
            try JSONSerialization.jsonObject(with: JSONEncoder().encode(command)) as! [String: Any]
        }
        let edit = try json(.editSet(setId: "s", weight: "62,5", reps: 8, seconds: 0, distanceMeters: 0, kind: "warmup"))
        XCTAssertEqual(edit["type"] as? String, "edit_set")
        XCTAssertEqual(edit["weight"] as? String, "62,5")
        XCTAssertEqual(edit["distance_meters"] as? Int, 0)
        let start = try json(.startWorkout(title: "Workout", routineId: nil))
        XCTAssertEqual(start.keys.sorted(), ["title", "type"])
        let routine = try json(.saveRoutine(name: "A", exerciseIds: nil, fromActive: true))
        XCTAssertEqual(routine["from_active"] as? Bool, true)

        let envelope = try JSONSerialization.jsonObject(with: JSONEncoder().encode(
            EngineRequest(opId: "op", expectedRevision: 12, timezone: "Europe/Rome", command: GymCommand.finishWorkout))) as! [String: Any]
        XCTAssertEqual(envelope["op_id"] as? String, "op")
        XCTAssertEqual(envelope["expected_revision"] as? Int, 12)
        XCTAssertEqual(envelope["timezone"] as? String, "Europe/Rome")
        XCTAssertEqual((envelope["command"] as? [String: Any])?["type"] as? String, "finish_workout")
    }

    func testBodyViewDocShape() throws {
        let view = try Fixtures.decode(BodyView.self, """
        {"revision":3,"settings":{"unit":"kg","goal_grams":78000},
         "entries":[{"id":"e2","weight_grams":80500,"measured_ms":1792584000000,"note":"","created_ms":1,"updated_ms":2}],
         "stats":{"count":3,"current_grams":80500,"current_measured_ms":1792584000000,"avg7_grams":80800,
                  "weekly_change_grams":-400,"min_grams":80500,"max_grams":81200,"trend30_grams":-700,"trend90_grams":null,
                  "goal_grams":78000,"goal_distance_grams":2800,"goal_reached":false},
         "series":[{"entry_id":"e1","measured_ms":1792411200000,"weight_grams":81200,"avg7_grams":81200}],
         "labels":{"e2":"80.5 kg"},
         "stat_labels":{"current":"80.5 kg","avg7":"80.8 kg","weekly_change":"-0.4 kg"}}
        """)
        XCTAssertEqual(view.revision, 3)
        XCTAssertEqual(view.settings, BodySettings(unit: .kg, goalGrams: 78000))
        XCTAssertEqual(view.stats.count, 3)
        XCTAssertNil(view.stats.trend90Grams)
        XCTAssertEqual(view.stats.goalReached, false)
        XCTAssertEqual(view.series?.first?.entryId, "e1")
        XCTAssertEqual(view.label(for: view.entries[0]), "80.5 kg")
        XCTAssertEqual(view.statLabel("weekly_change"), "-0.4 kg")
        // Missing labels fall back to the same formatting.
        XCTAssertEqual(view.statLabel("goal_distance"), "+2.8 kg")
        XCTAssertNil(view.statLabel("trend90"))
    }

    func testBodyViewMockShapeAndEnvelope() throws {
        // The web mock's older shape: no labels, no count, envelope around the view.
        let response = try BodyClient.decodeStateBody(Data("""
        {"ok":true,"replayed":false,"view":{"revision":0,"settings":{"unit":"lb","goal_grams":null},
          "entries":[{"id":"a","weight_grams":80000,"measured_ms":1792600000000,"note":"Morning"}],
          "stats":{"current_grams":80000,"avg7_grams":80000,"weekly_change_grams":null,"min_grams":80000,"max_grams":80000,
                   "trend30_grams":null,"trend90_grams":null,"goal_distance_grams":null}}}
        """.utf8))
        let view = try XCTUnwrap(response.view)
        XCTAssertEqual(view.unit, .lb)
        XCTAssertNil(view.settings.goalGrams)
        XCTAssertNil(view.series)
        XCTAssertEqual(view.label(for: view.entries[0]), "176.4 lb")
        XCTAssertEqual(view.statLabel("current"), "176.4 lb")

        // Revision and entries nested under `state` are tolerated.
        let nested = try Fixtures.decode(BodyView.self, #"{"state":{"revision":5,"settings":{"unit":"kg"},"entries":[]},"stats":{}}"#)
        XCTAssertEqual(nested.revision, 5)
        XCTAssertThrowsError(try Fixtures.decode(BodyView.self, #"{"entries":[]}"#))
    }

    func testBodyCommandEncoding() throws {
        func json(_ command: BodyCommand) throws -> [String: Any] {
            try JSONSerialization.jsonObject(with: JSONEncoder().encode(command)) as! [String: Any]
        }
        let log = try json(.logEntry(weightGrams: 80500, measuredMs: 1, note: ""))
        XCTAssertEqual(log["type"] as? String, "log_entry")
        XCTAssertEqual(log["weight_grams"] as? Int, 80500)
        let clear = try json(.updateSettings(unit: .kg, goalGrams: nil))
        XCTAssertTrue(clear["goal_grams"] is NSNull, "a cleared goal is an explicit null")
    }

    func testWeightParsingMirrorsTheEngine() {
        XCTAssertEqual(BodyUnit.kg.parse("80.5"), 80500)
        XCTAssertEqual(BodyUnit.kg.parse("80,25"), 80250)
        XCTAssertEqual(BodyUnit.kg.parse(" 80 "), 80000)
        // 176.5 lb × 453.59237 = 80 059.05 g → 80059, the docs/server-api.md example.
        XCTAssertEqual(BodyUnit.lb.parse("176,5"), 80059)
        XCTAssertEqual(BodyUnit.lb.parse("1"), 454)
        XCTAssertNil(BodyUnit.kg.parse("80.1234"))
        XCTAssertNil(BodyUnit.kg.parse("-1"))
        XCTAssertNil(BodyUnit.kg.parse("1.2.3"))
        XCTAssertNil(BodyUnit.kg.parse(".5"))
        XCTAssertEqual(BodyUnit.kg.signedLabel(grams: -400), "-0.4 kg")
        XCTAssertEqual(BodyUnit.kg.signedLabel(grams: 1200), "+1.2 kg")
        XCTAssertEqual(BodyUnit.kg.label(grams: 80000), "80 kg")
    }

    func testChatMessageCleaningAndLinks() {
        XCTAssertEqual(ChatStore.clean("  hi\r\nthere\u{0007}\t! "), "hi\nthere\t!")
        XCTAssertTrue(ChatStore.isValidSessionId("ios-abc_1"))
        XCTAssertFalse(ChatStore.isValidSessionId("bad id"))

        let text = "See https://example.com/recipes/risotto (the one you liked) and www.example.org. javascript:alert(1) is not a link."
        let links = LinkText.links(in: text).map(\.url.absoluteString)
        XCTAssertEqual(links.first, "https://example.com/recipes/risotto")
        XCTAssertFalse(links.contains { $0.hasPrefix("javascript") })
        let attributed = LinkText.attributed("<b>bold</b> **not markdown**")
        XCTAssertEqual(String(attributed.characters), "<b>bold</b> **not markdown**")
    }
}
