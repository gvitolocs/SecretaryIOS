package tech.getsecretary.secretary;
import android.os.*;import android.content.*;import android.net.Uri;import android.widget.*;import android.util.Base64;import org.json.*;import java.io.*;
public final class TranscriptActivity extends Ui {
 TextView status;String selectedId;
 public void onCreate(Bundle b){super.onCreate(b);render();}
 void render(){screen("Calls · Transcripts");status=text("Private history on this phone · latest 100 calls",13);button("Import a call recording",()->{selectedId=null;choose();});
 try{JSONArray rows=NativeCalls.call(this,"history",new JSONObject()).getJSONArray("records");if(rows.length()==0)text("No calls recorded yet.",17);
  for(int i=0;i<rows.length();i++){JSONObject r=rows.getJSONObject(i);text(r.optString("peer")+" · "+r.optString("kind"),21);text(new java.util.Date(r.optLong("startedAt")).toString()+" · "+r.optString("status"),12);JSONArray turns=r.optJSONArray("turns");if(turns!=null&&turns.length()>0){StringBuilder text=new StringBuilder();for(int n=0;n<turns.length();n++){JSONObject t=turns.getJSONObject(n);text.append(t.optString("role")).append(": ").append(t.optString("text")).append("\n\n");}TextView view=text(text.toString(),16);view.setTextIsSelectable(true);}else text(r.optString("kind").equals("sim")?"SIM audio is unavailable to this app. Import an existing recording for a transcript.":"No speech transcript was saved.",13);
   final String id=r.getString("id");button("Attach recording to this call",()->{selectedId=id;choose();});
  }
 }catch(Exception e){status.setText(e.getMessage());}
 }
 void choose(){new android.app.AlertDialog.Builder(this).setTitle("Transcribe recording").setMessage("Choose an audio recording up to 8 MiB. Secretary sends it to the configured Gemini transcription model. The transcript is kept in your private call history.").setNegativeButton("Cancel",null).setPositiveButton("Choose recording",(d,w)->{Intent i=new Intent(Intent.ACTION_OPEN_DOCUMENT).setType("audio/*").addCategory(Intent.CATEGORY_OPENABLE);startActivityForResult(i,5);}).show();}
 public void onActivityResult(int request,int result,Intent data){super.onActivityResult(request,result,data);if(request!=5||result!=RESULT_OK||data==null||data.getData()==null)return;Uri uri=data.getData();status.setText("Transcribing…");String id=selectedId;
 new Thread(()->{try{ByteArrayOutputStream bytes=new ByteArrayOutputStream();try(InputStream in=getContentResolver().openInputStream(uri)){byte[]buffer=new byte[8192];int n;while((n=in.read(buffer))!=-1){if(bytes.size()+n>8*1024*1024)throw new IOException("Recording exceeds 8 MiB.");bytes.write(buffer,0,n);}}
 String mime=getContentResolver().getType(uri);if("audio/mp4".equals(mime))mime="audio/m4a";JSONObject transcript=NativeCalls.call(this,"transcribe",new JSONObject().put("audioBase64",Base64.encodeToString(bytes.toByteArray(),Base64.NO_WRAP)).put("mimeType",mime).put("consent",true));String call=id==null?NativeCalls.begin(this,"recording","Imported recording"):id;String text=transcript.getString("transcript");if(transcript.optBoolean("partial"))text="[Partial transcript]\n"+text;NativeCalls.append(this,call,"recording",text);if(id==null)NativeCalls.end(this,call,"transcribed");runOnUiThread(this::render);
 }catch(Exception e){runOnUiThread(()->status.setText(e.getMessage()));}},"lucina-transcribe").start();
 }
}
