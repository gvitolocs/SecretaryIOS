package tech.getsecretary.lucinaotp;
import javax.net.ssl.HttpsURLConnection;import android.content.*;import java.net.*;import java.io.*;import java.nio.charset.StandardCharsets;import org.json.*;
final class NetForwarder {
 static final String ORIGIN="https://getsecretary.tech/v1/phone/otp";
 static JSONObject request(Context c,String path,JSONObject body)throws Exception{
  String token=c.getSharedPreferences("bridge",0).getString("internetToken","");
  if(token.length()<40)throw new IOException("Not paired");
  HttpsURLConnection conn=(HttpsURLConnection)new URL(ORIGIN+path).openConnection();
  conn.setConnectTimeout(2000);conn.setReadTimeout(2000);conn.setInstanceFollowRedirects(false);
  conn.setRequestProperty("Authorization","Bearer "+token);
  conn.setRequestProperty("Content-Type","application/json");
  try{
   if(body!=null){conn.setRequestMethod("POST");conn.setDoOutput(true);try(OutputStream out=conn.getOutputStream()){out.write(body.toString().getBytes(StandardCharsets.UTF_8));}}
   if(conn.getResponseCode()!=200)throw new IOException("http_"+conn.getResponseCode());
   ByteArrayOutputStream bytes=new ByteArrayOutputStream();try(InputStream in=conn.getInputStream()){
    byte[]buffer=new byte[1024];int n;while((n=in.read(buffer))!=-1){if(bytes.size()+n>4096)throw new IOException("Response limit");bytes.write(buffer,0,n);}
   }
   return new JSONObject(new String(bytes.toByteArray(),StandardCharsets.UTF_8));
  }finally{conn.disconnect();}
 }
 static String forward(Context c,String sender,String body){
  try{
   JSONObject window=request(c,"/window",null);if(!window.optBoolean("armed"))return "paused";
   String code=NativeOtp.parse(sender==null?"":sender,body);if(code==null)return "not_otp";
   JSONObject event=new JSONObject().put("windowId",window.getString("windowId")).put("eventId",java.util.UUID.randomUUID().toString()).put("code",code).put("sender",sender==null?"SMS":sender);
   request(c,"/incoming",event);return "accepted";
  }catch(Exception error){return error.getClass().getSimpleName();/* No SMS content or codes in logs or disk queues. */}
 }
}
