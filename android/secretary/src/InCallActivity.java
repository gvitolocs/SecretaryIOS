package tech.getsecretary.secretary;
import android.os.*;import android.content.*;import android.telecom.*;import android.widget.*;
public final class InCallActivity extends Ui {
 private final BroadcastReceiver changed=new BroadcastReceiver(){public void onReceive(Context c,Intent i){render();}};
 public void onCreate(Bundle b){super.onCreate(b);setShowWhenLocked(true);setTurnScreenOn(true);if(Build.VERSION.SDK_INT>=33)registerReceiver(changed,new IntentFilter("tech.getsecretary.secretary.CALL_CHANGED"),Context.RECEIVER_NOT_EXPORTED);else registerReceiver(changed,new IntentFilter("tech.getsecretary.secretary.CALL_CHANGED"));render();}
 void render(){screen("Lucina · Phone");
 if(LucinaCallService.calls.isEmpty()){text("Call ended",22);button("View transcript",()->startActivity(new Intent(this,TranscriptActivity.class)));return;}
 for(Call c:LucinaCallService.calls){text(LucinaCallService.peer(c),24);text(state(c.getState()),15);
  if(c.getState()==Call.STATE_RINGING){button("Answer",()->c.answer(c.getDetails().getVideoState()));button("Decline",()->c.reject(false,null));}
  else{button("End call",c::disconnect);if(c.getState()==Call.STATE_HOLDING)button("Resume",c::unhold);else if(c.getDetails().can(Call.Details.CAPABILITY_HOLD))button("Hold",c::hold);}
 }
 button("Mute microphone",()->{if(LucinaCallService.live!=null)LucinaCallService.live.setMuted(true);});button("Unmute",()->{if(LucinaCallService.live!=null)LucinaCallService.live.setMuted(false);});
 button("Speaker",()->{if(LucinaCallService.live!=null)LucinaCallService.live.setAudioRoute(CallAudioState.ROUTE_SPEAKER);});button("Earpiece",()->{if(LucinaCallService.live!=null)LucinaCallService.live.setAudioRoute(CallAudioState.ROUTE_EARPIECE);});
 EditText tones=new EditText(this);tones.setHint("Keypad digits");tones.setInputType(android.text.InputType.TYPE_CLASS_PHONE);body.addView(tones);button("Send keypad digits",()->{if(LucinaCallService.calls.isEmpty())return;Call c=LucinaCallService.calls.get(0);String s=tones.getText().toString();Handler h=new Handler(getMainLooper());for(int i=0;i<s.length()&&i<30;i++){final char tone=s.charAt(i);if("0123456789*#".indexOf(tone)<0)continue;h.postDelayed(()->{c.playDtmfTone(tone);h.postDelayed(c::stopDtmfTone,120);},i*250L);}});
 }
 static String state(int s){switch(s){case Call.STATE_RINGING:return "Incoming";case Call.STATE_ACTIVE:return "Connected";case Call.STATE_DIALING:return "Dialing";case Call.STATE_HOLDING:return "On hold";case Call.STATE_DISCONNECTED:return "Disconnected";default:return "Connecting";}}
 public void onDestroy(){unregisterReceiver(changed);super.onDestroy();}
}
