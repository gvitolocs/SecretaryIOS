package tech.getsecretary.secretary;
import android.app.*;import android.content.*;import android.telecom.*;import android.os.*;import java.util.*;import java.util.concurrent.CopyOnWriteArrayList;
public final class LucinaCallService extends InCallService {
 static LucinaCallService live;static final List<Call> calls=new CopyOnWriteArrayList<>();private final Map<Call,String> ids=new HashMap<>();
 public void onCreate(){super.onCreate();live=this;getSystemService(NotificationManager.class).createNotificationChannel(new NotificationChannel("calls","Phone calls",NotificationManager.IMPORTANCE_HIGH));}
 public void onCallAdded(Call call){super.onCallAdded(call);calls.add(call);try{ids.put(call,NativeCalls.begin(this,"sim",peer(call)));}catch(Exception ignored){}
 call.registerCallback(new Call.Callback(){public void onStateChanged(Call c,int state){notifyCall();sendBroadcast(new Intent("tech.getsecretary.secretary.CALL_CHANGED").setPackage(getPackageName()));}});notifyCall();}
 static String peer(Call c){try{return c.getDetails().getHandle()==null?"Private number":c.getDetails().getHandle().getSchemeSpecificPart();}catch(Exception e){return "Phone call";}}
 private void notifyCall(){
  if(calls.isEmpty())return;
  Call c=calls.stream().filter(v->v.getState()==Call.STATE_RINGING).findFirst().orElse(calls.get(0));
  Intent ui=new Intent(this,InCallActivity.class).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK|Intent.FLAG_ACTIVITY_SINGLE_TOP);
  PendingIntent pending=PendingIntent.getActivity(this,1,ui,PendingIntent.FLAG_UPDATE_CURRENT|PendingIntent.FLAG_IMMUTABLE);
  Notification.Builder n=new Notification.Builder(this,"calls").setSmallIcon(android.R.drawable.sym_action_call).setContentTitle(c.getState()==Call.STATE_RINGING?"Incoming call":"Phone call").setContentText(peer(c)).setCategory(Notification.CATEGORY_CALL).setOngoing(true).setContentIntent(pending);
  if(c.getState()==Call.STATE_RINGING)n.setFullScreenIntent(pending,true);
  getSystemService(NotificationManager.class).notify(2001,n.build());
 }
 public void onCallRemoved(Call call){String id=ids.remove(call);if(id!=null)NativeCalls.end(this,id,"disconnected");calls.remove(call);super.onCallRemoved(call);if(calls.isEmpty())getSystemService(NotificationManager.class).cancel(2001);else notifyCall();sendBroadcast(new Intent("tech.getsecretary.secretary.CALL_CHANGED").setPackage(getPackageName()));}
 public void onDestroy(){live=null;super.onDestroy();}
}
