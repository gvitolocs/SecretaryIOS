package tech.getsecretary.lucinaotp;
import android.content.*;import android.database.Cursor;import android.net.Uri;import android.os.*;import java.io.*;import java.nio.charset.StandardCharsets;import org.json.*;
public final class PairingProvider extends ContentProvider {
 public boolean onCreate(){return true;}
 private void authorize(){int uid=Binder.getCallingUid();if(uid!=2000&&uid!=0&&uid!=android.os.Process.myUid())throw new SecurityException("Authenticated ADB pairing only");}
 public Bundle call(String method,String arg,Bundle extras){
  authorize();if("cleanup".equals(method)){MainActivity.cleanupLegacyNotification(getContext());Bundle result=new Bundle();result.putBoolean("cleaned",true);return result;}if("selftest".equals(method)){String result=NetForwarder.forward(getContext(),"LucinaTest","Your verification code is 482917.");Bundle test=new Bundle();test.putString("result",result);test.putBoolean("submitted","accepted".equals(result));return test;}if(!"status".equals(method))throw new IllegalArgumentException("Unknown pairing method");
  Bundle b=new Bundle();b.putBoolean("paired",getContext().getSharedPreferences("bridge",0).getString("internetToken","").length()>=40);b.putString("version","0.4.0");b.putBoolean("rustSelfTest","482917".equals(NativeOtp.parse("Test","Your verification code is 482917.")));return b;
 }
 public ParcelFileDescriptor openFile(Uri uri,String mode)throws FileNotFoundException{
  authorize();if(!"w".equals(mode)||!"/configure".equals(uri.getPath()))throw new FileNotFoundException();
  try{
   ParcelFileDescriptor[]pipe=ParcelFileDescriptor.createPipe();
   new Thread(()->{try(InputStream in=new ParcelFileDescriptor.AutoCloseInputStream(pipe[0])){
    ByteArrayOutputStream out=new ByteArrayOutputStream();byte[]buffer=new byte[512];int n;
    while((n=in.read(buffer))!=-1){if(out.size()+n>2048)return;out.write(buffer,0,n);}
    JSONObject data=new JSONObject(new String(out.toByteArray(),StandardCharsets.UTF_8));String token=data.getString("token");
    if(!NetForwarder.ORIGIN.equals(data.getString("url"))||!token.matches("[0-9a-f]{64}"))return;
    getContext().getSharedPreferences("bridge",0).edit().putString("internetToken",token).commit();
   }catch(Exception ignored){} },"lucina-pairing").start();
   return pipe[1];
  }catch(IOException e){throw new FileNotFoundException("Pairing unavailable");}
 }
 private SecurityException denied(){return new SecurityException("No SMS data interface");}
 public Cursor query(Uri u,String[]p,String s,String[]a,String o){throw denied();}public String getType(Uri u){throw denied();}public Uri insert(Uri u,ContentValues v){throw denied();}public int delete(Uri u,String s,String[]a){throw denied();}public int update(Uri u,ContentValues v,String s,String[]a){throw denied();}
}
