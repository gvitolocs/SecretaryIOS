package tech.getsecretary.lucinaotp;
import android.content.*;import android.provider.Telephony;import android.telephony.SmsMessage;
public final class SmsReceiver extends BroadcastReceiver {
 public void onReceive(Context context,Intent intent){
  if(!Telephony.Sms.Intents.SMS_RECEIVED_ACTION.equals(intent.getAction())||!context.getSharedPreferences("bridge",0).getBoolean("enabled",false))return;
  StringBuilder body=new StringBuilder();String sender=null;
  for(SmsMessage sms:Telephony.Sms.Intents.getMessagesFromIntent(intent)){if(sender==null)sender=sms.getOriginatingAddress();body.append(sms.getMessageBody());}
  if(body.length()>2048)return;
  final String from=sender,text=body.toString();final PendingResult pending=goAsync();final Context app=context.getApplicationContext();
  new Thread(()->{try{NetForwarder.forward(app,from,text);}finally{pending.finish();}},"lucina-otp-event").start();
 }
}
