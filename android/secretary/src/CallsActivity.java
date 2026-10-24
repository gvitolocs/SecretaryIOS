package tech.getsecretary.secretary;
import android.Manifest;import android.app.role.RoleManager;import android.os.*;import android.content.*;import android.content.pm.PackageManager;import android.telecom.*;import android.net.Uri;import android.widget.*;import org.json.*;
public final class CallsActivity extends Ui {
 EditText number;
 public void onCreate(Bundle b){super.onCreate(b);screen("Lucina · Calls");text("Phone calls, conversations with Lucina, and private transcripts.",15);
 button("Talk to Lucina · internet",()->startActivity(new Intent(this,VoiceActivity.class)));
 button("Set up SIM call controls",()->{RoleManager roles=getSystemService(RoleManager.class);if(roles.isRoleAvailable(RoleManager.ROLE_DIALER)&&!roles.isRoleHeld(RoleManager.ROLE_DIALER))startActivityForResult(roles.createRequestRoleIntent(RoleManager.ROLE_DIALER),1);else {notice("Secretary already handles phone calls.");notifications();}});
 text("Phone number",14);number=new EditText(this);number.setTextColor(-1);number.setInputType(android.text.InputType.TYPE_CLASS_PHONE);body.addView(number);if(getIntent().getData()!=null)number.setText(getIntent().getData().getSchemeSpecificPart());
 button("Call this number",this::dial);button("Call history & transcripts",()->startActivity(new Intent(this,TranscriptActivity.class)));
 text("SIM calls use Android’s phone role. Recording both sides is not available to a normal app. Import an existing call recording to transcribe it. Internet conversations here are with Lucina; other VoIP services are not connected.",13);
 }
 void notifications(){if(Build.VERSION.SDK_INT>=33&&checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS)!=PackageManager.PERMISSION_GRANTED)requestPermissions(new String[]{Manifest.permission.POST_NOTIFICATIONS},4);}
 public void onActivityResult(int request,int result,Intent data){super.onActivityResult(request,result,data);if(request==1&&result==RESULT_OK)notifications();}
 private void dial(){
  String n=number.getText().toString().trim();if(!n.matches("[+0-9*# ,;()-]{1,60}")){notice("Enter a phone number.");return;}
  if(checkSelfPermission(Manifest.permission.CALL_PHONE)!=PackageManager.PERMISSION_GRANTED){requestPermissions(new String[]{Manifest.permission.CALL_PHONE},2);return;}
  try{JSONObject result=new JSONObject(NativeCalls.phoneCall(this,n,false));notice(result.optString("message",result.optString("status")));}catch(Exception e){notice("Phone function failed.");}
 }
 public void onRequestPermissionsResult(int r,String[]p,int[]g){super.onRequestPermissionsResult(r,p,g);if(r==2&&g.length>0&&g[0]==PackageManager.PERMISSION_GRANTED)notice("Call permission enabled. Tap Call this number when ready.");}
}
