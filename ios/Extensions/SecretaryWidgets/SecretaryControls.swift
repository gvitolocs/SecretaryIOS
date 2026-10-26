import AppIntents
import SwiftUI
import WidgetKit

// Control Center, Lock Screen and Action button controls (iOS 18): each opens Secretary where the
// action happens (the compose field, the add field, the weight sheet). Recording a memo is the
// memos feature's own control (MemoRecordControl), which records without opening the app.

struct AskLucinaControl: ControlWidget {
    var body: some ControlWidgetConfiguration {
        StaticControlConfiguration(kind: "tech.getsecretary.control.ask") {
            ControlWidgetButton(action: OpenSecretaryIntent("chat/compose")) {
                Label("Ask Lucina", systemImage: "bubble.left.and.text.bubble.right.fill")
            }
        }
        .displayName("Ask Lucina")
        .description("Opens the chat ready to type.")
    }
}

struct AddToShoppingControl: ControlWidget {
    var body: some ControlWidgetConfiguration {
        StaticControlConfiguration(kind: "tech.getsecretary.control.shopping") {
            ControlWidgetButton(action: OpenSecretaryIntent("shopping/add")) {
                Label("Add to shopping", systemImage: "cart.badge.plus")
            }
        }
        .displayName("Add to shopping")
        .description("Opens the shopping list ready to add an item.")
    }
}

struct LogWeightControl: ControlWidget {
    var body: some ControlWidgetConfiguration {
        StaticControlConfiguration(kind: "tech.getsecretary.control.weight") {
            ControlWidgetButton(action: OpenSecretaryIntent("weight/log")) {
                Label("Log weight", systemImage: "scalemass.fill")
            }
        }
        .displayName("Log weight")
        .description("Opens the quick weight entry.")
    }
}
