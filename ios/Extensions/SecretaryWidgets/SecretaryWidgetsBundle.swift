import SwiftUI
import WidgetKit

/// Everything the extension offers: Home Screen, Lock Screen and StandBy widgets, the Control
/// Center controls, and the Live Activities (the voice-memo recording, an approval request).
@main
struct SecretaryWidgets: WidgetBundle {
    var body: some Widget {
        ApprovalsWidget()
        AskLucinaWidget()
        AppointmentWidget()
        CountdownWidget()
        ShoppingWidget()
        TasksWidget()
        WeightWidget()
        TrainingWidget()
        MemoWidget()
        PokemonWidget()
        FilmsWidget()
        ServerWidget()
        AskLucinaControl()
        AddToShoppingControl()
        MemoRecordControl()
        LogWeightControl()
        MemoRecordingLiveActivity()
        ApprovalLiveActivity()
    }
}
