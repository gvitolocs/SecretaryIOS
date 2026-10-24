package tech.getsecretary.lucinaotp;
import android.Manifest;import android.app.*;import android.os.*;import android.content.*;import android.content.pm.PackageManager;import android.widget.*;
public final class MainActivity extends Activity {
 private TextView status;
 public void onCreate(Bundle b){super.onCreate(b);cleanupLegacyNotification(this);LinearLayout panel=new LinearLayout(this);panel.setOrientation(1);panel.setPadding(32,48,32,32);setContentView(panel);
 TextView title=new TextView(this);title.setText("Lucina OTP");title.setTextSize(28);panel.addView(title);
 TextView description=new TextView(this);description.setText("Internet verification forwarding\n\nNew OTP messages from any provider, forwarded automatically while enabled. Android wakes this receiver for an SMS; it sends over HTTPS and exits. No continuously running service, kernel modification, SMS history, contacts or sending permission.\n\nParsing and verification state run in Rust. Codes expire within two minutes and can be delivered once to your private Lucina chat. Match the code to the service you requested.\n");panel.addView(description);
 status=new TextView(this);panel.addView(status);
 Button enable=new Button(this);enable.setText("Enable OTP forwarding");panel.addView(enable);enable.setOnClickListener(v->{
  if(checkSelfPermission(Manifest.permission.RECEIVE_SMS)!=PackageManager.PERMISSION_GRANTED)requestPermissions(new String[]{Manifest.permission.RECEIVE_SMS},1);else enable();
 });
 Button stop=new Button(this);stop.setText("Disable forwarding");panel.addView(stop);stop.setOnClickListener(v->{getSharedPreferences("bridge",0).edit().putBoolean("enabled",false).apply();refresh();});refresh();
 }
 static void cleanupLegacyNotification(Context c){NotificationManager nm=(NotificationManager)c.getSystemService(Context.NOTIFICATION_SERVICE);if(nm!=null){nm.cancelAll();if(Build.VERSION.SDK_INT>=26)nm.deleteNotificationChannel("otp");}}
 private void enable(){getSharedPreferences("bridge",0).edit().putBoolean("enabled",true).apply();refresh();}
 private void refresh(){boolean paired=getSharedPreferences("bridge",0).getString("internetToken","").length()>=40;status.setText("Incoming SMS: "+(checkSelfPermission(Manifest.permission.RECEIVE_SMS)==PackageManager.PERMISSION_GRANTED?"allowed":"not allowed")+"\nForwarding: "+(getSharedPreferences("bridge",0).getBoolean("enabled",false)?"enabled":"off")+"\nInternet pairing: "+(paired?"ready":"not configured")+"\n");}
 public void onRequestPermissionsResult(int r,String[]p,int[]g){super.onRequestPermissionsResult(r,p,g);if(r==1&&checkSelfPermission(Manifest.permission.RECEIVE_SMS)==PackageManager.PERMISSION_GRANTED)enable();else refresh();}
}
