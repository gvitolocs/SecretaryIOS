package tech.getsecretary.secretary;
import android.content.*;import android.webkit.CookieManager;import org.json.*;
final class NativeCalls {
 static{System.loadLibrary("lucina_device");}
 static native String phoneCall(Context context,String number,boolean dryRun);
 private static native String run(String dir,String op,String json,String cookie);
 static JSONObject call(Context c,String op,JSONObject value)throws Exception{
  String cookie=CookieManager.getInstance().getCookie("https://getsecretary.tech");
  JSONObject result=new JSONObject(run(c.getFilesDir().getAbsolutePath(),op,value.toString(),cookie==null?"":cookie));
  if(!result.optBoolean("ok"))throw new Exception(result.optString("error","Call operation unavailable"));
  return result.getJSONObject("data");
 }
 static String begin(Context c,String kind,String peer)throws Exception{return call(c,"begin",new JSONObject().put("kind",kind).put("peer",peer)).getString("id");}
 static void append(Context c,String id,String role,String text)throws Exception{call(c,"append",new JSONObject().put("id",id).put("role",role).put("text",text));}
 static void end(Context c,String id,String status){try{call(c,"end",new JSONObject().put("id",id).put("status",status));}catch(Exception ignored){}}
}
