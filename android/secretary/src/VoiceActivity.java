package tech.getsecretary.secretary;
import android.Manifest;import android.os.*;import android.content.*;import android.content.pm.PackageManager;import android.speech.*;import android.speech.tts.*;import android.telecom.TelecomManager;import android.widget.*;import java.util.*;import org.json.*;
public final class VoiceActivity extends Ui implements RecognitionListener {
 SpeechRecognizer recognizer;TextToSpeech tts;TextView status,dialogue;String id;boolean ended=false,ready=false;
 public void onCreate(Bundle b){super.onCreate(b);screen("Call Lucina · Internet");status=text("Starting…",18);dialogue=text("",16);button("Hang up",this::hangup);text("This is a voice conversation with Lucina over your internet connection. Speech recognition uses the phone’s configured speech service. Your transcript appears in Calls afterwards. This does not record a SIM call.",13);
 if(checkSelfPermission(Manifest.permission.READ_PHONE_STATE)!=PackageManager.PERMISSION_GRANTED){requestPermissions(new String[]{Manifest.permission.RECORD_AUDIO,Manifest.permission.READ_PHONE_STATE},3);return;}
 if(getSystemService(TelecomManager.class).isInCall()){status.setText("Finish the phone call before starting a conversation with Lucina.");return;}
 if(checkSelfPermission(Manifest.permission.RECORD_AUDIO)!=PackageManager.PERMISSION_GRANTED){requestPermissions(new String[]{Manifest.permission.RECORD_AUDIO},3);return;}start();
 }
 void start(){if(checkSelfPermission(Manifest.permission.READ_PHONE_STATE)!=PackageManager.PERMISSION_GRANTED||getSystemService(TelecomManager.class).isInCall()){status.setText("Phone state access is required, and a SIM call must finish first.");return;}if(!SpeechRecognizer.isRecognitionAvailable(this)){status.setText("Speech recognition is unavailable. Use Lucina’s text chat.");return;}
 try{id=NativeCalls.begin(this,"internet","Lucina");}catch(Exception e){status.setText(e.getMessage());return;}
 recognizer=SpeechRecognizer.createSpeechRecognizer(this);recognizer.setRecognitionListener(this);
 tts=new TextToSpeech(this,code->{ready=code==TextToSpeech.SUCCESS;if(ready){tts.setLanguage(Locale.getDefault());tts.setOnUtteranceProgressListener(new UtteranceProgressListener(){public void onStart(String u){}public void onDone(String u){runOnUiThread(()->listen());}public void onError(String u){runOnUiThread(()->status.setText("Voice playback failed. Tap Listen to continue."));}});}listen();});
 button("Listen",this::listen);
 }
 void listen(){if(ended||recognizer==null)return;status.setText("Listening…");Intent i=new Intent(RecognizerIntent.ACTION_RECOGNIZE_SPEECH);i.putExtra(RecognizerIntent.EXTRA_LANGUAGE_MODEL,RecognizerIntent.LANGUAGE_MODEL_FREE_FORM);i.putExtra(RecognizerIntent.EXTRA_LANGUAGE,Locale.getDefault().toLanguageTag());i.putExtra(RecognizerIntent.EXTRA_PARTIAL_RESULTS,false);recognizer.startListening(i);}
 void line(String role,String text){dialogue.append((role.equals("user")?"You: ":"Lucina: ")+text+"\n\n");try{NativeCalls.append(this,id,role,text);}catch(Exception ignored){}}
 public void onResults(Bundle b){ArrayList<String> text=b.getStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION);if(ended||text==null||text.isEmpty())return;String heard=text.get(0);line("user",heard);status.setText("Lucina is thinking…");
 new Thread(()->{try{JSONObject reply=NativeCalls.call(this,"reply",new JSONObject().put("text",heard));String answer=reply.optString("reply","No reply was returned.");runOnUiThread(()->{if(ended)return;line("lucina",answer);status.setText("Lucina");if(ready&&!reply.optBoolean("sensitive"))tts.speak(answer,TextToSpeech.QUEUE_FLUSH,null,"reply");else status.setText(answer+"\nTap Listen to continue.");});}catch(Exception e){runOnUiThread(()->{if(!ended)status.setText(e.getMessage()+"\nTap Listen to retry.");});}},"lucina-voice").start();
 }
 void hangup(){if(ended)return;ended=true;if(id!=null)NativeCalls.end(this,id,"ended");if(recognizer!=null)recognizer.cancel();if(tts!=null)tts.stop();startActivity(new Intent(this,TranscriptActivity.class));finish();}
 public void onRequestPermissionsResult(int r,String[]p,int[]g){super.onRequestPermissionsResult(r,p,g);if(r==3&&g.length>0&&g[0]==PackageManager.PERMISSION_GRANTED)start();else status.setText("Microphone permission is required. Text chat remains available.");}
 public void onError(int error){if(!ended)status.setText("Listening stopped ("+error+"). Tap Listen to continue.");}
 public void onReadyForSpeech(Bundle b){}public void onBeginningOfSpeech(){}public void onRmsChanged(float r){}public void onBufferReceived(byte[]b){}public void onEndOfSpeech(){status.setText("Recognizing…");}public void onPartialResults(Bundle b){}public void onEvent(int t,Bundle b){}
 public void onStop(){super.onStop();if(!isChangingConfigurations()){ended=true;if(id!=null)NativeCalls.end(this,id,"ended");if(recognizer!=null)recognizer.cancel();if(tts!=null)tts.stop();}}
 public void onDestroy(){if(recognizer!=null)recognizer.destroy();if(tts!=null)tts.shutdown();super.onDestroy();}
}
