package tech.getsecretary.secretary

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

/**
 * The notification buttons (Answer, Decline, Hang up). They run only when the owner taps them: nothing in this
 * app answers or rejects a call by itself. Not exported; only this app's own PendingIntents reach it.
 */
class CallActionReceiver : BroadcastReceiver() {
    override fun onReceive(c: Context, i: Intent) {
        when (i.action) {
            ANSWER -> LucinaCallService.answerRinging()
            DECLINE -> LucinaCallService.declineRinging()
            HANGUP -> LucinaCallService.hangUp()
        }
    }

    companion object {
        const val ANSWER = "tech.getsecretary.secretary.action.CALL_ANSWER"
        const val DECLINE = "tech.getsecretary.secretary.action.CALL_DECLINE"
        const val HANGUP = "tech.getsecretary.secretary.action.CALL_HANGUP"
    }
}
