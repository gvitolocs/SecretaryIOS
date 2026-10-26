import SwiftUI
import WidgetKit

/// Everything the extension offers: Home Screen, Lock Screen and StandBy widgets, the Control
/// Center controls, and the voice-memo recording Live Activity.
@main
struct SecretaryWidgets: WidgetBundle {
    var body: some Widget {
        AskLucinaWidget()
        AppointmentWidget()
        ShoppingWidget()
        TasksWidget()
        WeightWidget()
        TrainingWidget()
        MemoWidget()
        PokemonWidget()
        FilmsWidget()
        AskLucinaControl()
        AddToShoppingControl()
        RecordMemoControl()
        LogWeightControl()
        MemoRecordingLiveActivity()
    }
}
