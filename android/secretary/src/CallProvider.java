package tech.getsecretary.secretary;
import android.content.*;import android.database.Cursor;import android.net.Uri;import android.os.*;
/** Authenticated ADB invokes the same Rust function as the phone app's Call button. */
public final class CallProvider extends ContentProvider {
 public boolean onCreate(){return true;}
 public Bundle call(String method,String arg,Bundle extras){
  int uid=Binder.getCallingUid();if(uid!=2000&&uid!=0&&uid!=android.os.Process.myUid())throw new SecurityException("Owner-authorized device bridge only");
  if(!"call".equals(method)&&!"check".equals(method))throw new IllegalArgumentException("Unknown phone function");
  String result=NativeCalls.phoneCall(getContext(),arg==null?"":arg,"check".equals(method));
  Bundle b=new Bundle();b.putString("result",result);return b;
 }
 public Cursor query(Uri u,String[]p,String s,String[]a,String o){throw new SecurityException();}public String getType(Uri u){return null;}public Uri insert(Uri u,ContentValues v){throw new SecurityException();}public int delete(Uri u,String s,String[]a){throw new SecurityException();}public int update(Uri u,ContentValues v,String s,String[]a){throw new SecurityException();}
}
