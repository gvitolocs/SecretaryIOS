//! Private on-device call journal and fixed-origin voice/recording requests.
use serde_json::{json,Value};
use std::{fs,io::{Write,Read},os::unix::fs::OpenOptionsExt,sync::Mutex,time::{SystemTime,UNIX_EPOCH,Duration}};
static JOURNAL:Mutex<()>=Mutex::new(());
type R<T>=Result<T,String>;
fn now()->u64{SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64}
fn file(dir:&str)->R<std::path::PathBuf>{
 if !dir.ends_with("/tech.getsecretary.secretary/files"){return Err("Invalid app storage.".into())}
 Ok(std::path::Path::new(dir).join("calls.json"))
}
fn load(path:&std::path::Path)->R<Vec<Value>>{
 match fs::metadata(path){Ok(m)if m.len()>8*1024*1024=>Err("Call history exceeds limit.".into()),Ok(_)=>serde_json::from_slice(&fs::read(path).map_err(|_|"History unavailable.")?).map_err(|_|"History unavailable.".into()),Err(e)if e.kind()==std::io::ErrorKind::NotFound=>Ok(Vec::new()),Err(_)=>Err("History unavailable.".into())}
}
fn save(path:&std::path::Path,rows:&[Value])->R<()>{
 let temp=path.with_extension("json.tmp");let mut f=fs::OpenOptions::new().create(true).truncate(true).write(true).mode(0o600).open(&temp).map_err(|_|"History save failed.")?;
 let encoded=serde_json::to_vec(rows).map_err(|_|"History save failed.")?;
 if encoded.len()>8*1024*1024{return Err("Call history storage limit reached.".into())}
 f.write_all(&encoded).and_then(|_|f.sync_all()).map_err(|_|"History save failed.")?;
 fs::rename(temp,path).map_err(|_|"History save failed.".into())
}
pub fn journal(dir:&str,op:&str,v:&Value)->R<Value>{
 let _lock=JOURNAL.lock().map_err(|_|"History busy.")?;let path=file(dir)?;let mut rows=load(&path)?;
 if op=="history"{return Ok(json!({"records":rows.into_iter().rev().collect::<Vec<_>>()}))}
 if op=="begin"{
  let kind=v["kind"].as_str().filter(|s|["sim","internet","recording"].contains(s)).ok_or("Unknown call type.")?;
  let mut random=[0u8;12];fs::File::open("/dev/urandom").and_then(|mut f|f.read_exact(&mut random)).map_err(|_|"Call ID unavailable.")?;
  let id:String=random.iter().map(|b|format!("{b:02x}")).collect();
  let peer:String=v["peer"].as_str().unwrap_or("Lucina").chars().filter(|c|!c.is_control()).take(120).collect();
  rows.push(json!({"id":id,"kind":kind,"peer":peer,"startedAt":now(),"status":"active","turns":[]}));
  if rows.len()>100{rows.remove(0);}
  save(&path,&rows)?;return Ok(json!({"id":id}));
 }
 let row=rows.iter_mut().find(|r|r["id"]==v["id"]).ok_or("Call not found.")?;
 if op=="append"{
  let role=v["role"].as_str().filter(|s|["user","lucina","recording"].contains(s)).ok_or("Unknown speaker.")?;
  let text=v["text"].as_str().filter(|s|s.chars().count()<=40000).ok_or("Transcript limit.")?;
  if super::otp_intent(text).is_some()||super::parse_otp("Voice",text).is_some(){return Ok(json!({"saved":false,"sensitive":true}))}
  let turns=row["turns"].as_array_mut().ok_or("Invalid history.")?;
  if turns.len()>=300{return Err("Transcript limit.".into())}
  turns.push(json!({"role":role,"text":text,"at":now()}));
 }else if op=="end"{
  row["endedAt"]=json!(now());row["status"]=json!(v["status"].as_str().unwrap_or("ended").chars().take(40).collect::<String>());
 }else{return Err("Unknown call operation.".into())}
 save(&path,&rows)?;Ok(json!({"saved":true}))
}
#[cfg(any(feature="host",feature="android-network"))]
fn post(path:&str,cookie:&str,body:Value)->R<Value>{
 if cookie.is_empty(){return Err("Sign in to Secretary first.".into())}
 let agent=ureq::Agent::config_builder().tls_config(ureq::tls::TlsConfig::builder().unversioned_rustls_crypto_provider(std::sync::Arc::new(rustls_graviola::default_provider())).build()).timeout_global(Some(Duration::from_secs(120))).max_redirects(0).build().new_agent();
 let mut response=agent.post(format!("https://getsecretary.tech{path}")).header("Cookie",cookie).header("Origin","https://getsecretary.tech").header("X-Secretary-CSRF","1").header("X-Secretary-Client","web").send_json(body).map_err(|_|"The request failed. Check your sign-in, connection or provider availability.")?;
 let bytes=response.body_mut().with_config().limit(512*1024).read_to_vec().map_err(|_|"Response exceeded limit.")?;
 serde_json::from_slice(&bytes).map_err(|_|"Invalid response.".into())
}
#[cfg(any(feature="host",feature="android-network"))]
pub fn network(op:&str,v:&Value,cookie:&str)->R<Value>{
 if op=="reply"{
  let text=v["text"].as_str().filter(|s|!s.trim().is_empty()&&s.len()<=12000).ok_or("Invalid voice message.")?;
  if super::otp_intent(text).is_some()||super::parse_otp("Voice",text).is_some(){return Ok(json!({"reply":"Ask for verification codes in your private text chat.","sensitive":true}))}
  let value=post("/v1/chat",cookie,json!({"message":text,"sessionId":"android_voice","clientMessageId":format!("voice-{}-{}",now(),std::process::id()),"channel":"web"}))?;
  if value["sensitive"]==true{return Ok(json!({"reply":"A sensitive reply is available in your private chat.","sensitive":true}))}
  return Ok(value)
 }
 if op=="transcribe"{
  if v["consent"]!=true{return Err("Confirm sending this recording to Gemini.".into())}
  return post("/v1/calls/transcribe",cookie,v.clone())
 }
 Err("Unknown network operation.".into())
}
#[cfg(feature="android-network")]
#[no_mangle]
pub extern "system" fn Java_tech_getsecretary_secretary_NativeCalls_run(mut env:jni::JNIEnv,_class:jni::objects::JClass,dir:jni::objects::JString,op:jni::objects::JString,data:jni::objects::JString,cookie:jni::objects::JString)->jni::sys::jstring{
 let result=std::panic::catch_unwind(std::panic::AssertUnwindSafe(||->R<Value>{
  let dir:String=env.get_string(&dir).map_err(|_|"Storage unavailable.")?.into();
  let op:String=env.get_string(&op).map_err(|_|"Invalid operation.")?.into();
  let data:String=env.get_string(&data).map_err(|_|"Invalid request.")?.into();
  if data.len()>12*1024*1024{return Err("Request limit.".into())}
  let v:Value=serde_json::from_str(&data).map_err(|_|"Invalid request.")?;
  if ["reply","transcribe"].contains(&op.as_str()){
   let cookie:String=env.get_string(&cookie).map_err(|_|"Sign in first.")?.into();
   network(&op,&v,&cookie)
  }else{journal(&dir,&op,&v)}
 }));
 let text=match result{Ok(Ok(v))=>json!({"ok":true,"data":v}).to_string(),Ok(Err(e))=>json!({"ok":false,"error":e}).to_string(),Err(_)=>json!({"ok":false,"error":"Call operation failed."}).to_string()};
 env.new_string(text).map(|s|s.into_raw()).unwrap_or(std::ptr::null_mut())
}
#[cfg(test)]mod tests{
 use super::*;
 #[test]fn journal_is_confined_to_the_android_app(){assert!(file("/etc").is_err());assert!(file("/data/user/0/tech.getsecretary.secretary/files").is_ok());}
 #[test]fn no_voice_otp_request_is_sent_to_the_network(){assert_eq!(network("reply",&json!({"text":"show my OTP"}),"test-cookie").unwrap()["sensitive"],true);}
}


/// Normalize voice-call numbers only; never execute USSD or arbitrary URI schemes.
pub fn call_number(input:&str)->R<String>{
 let s:String=input.chars().filter(|c|!c.is_ascii_whitespace()&&!['(',')','-'].contains(c)).collect();
 let digits=s.strip_prefix('+').unwrap_or(&s);
 if !digits.bytes().all(|b|b.is_ascii_digit())||!(3..=15).contains(&digits.len())||s.matches('+').count()>1{return Err("Enter a valid phone number.".into())}
 if digits.len()<6&&!["112","911"].contains(&digits){return Err("Short service codes are not voice-call numbers.".into())}
 Ok(s)
}

/// Android implementation of Lucina's phone_call function. Telecom performs the call.
#[cfg(feature="android-network")]
fn phone_call(env:&mut jni::JNIEnv,context:&jni::objects::JObject,number:&str,dry_run:bool)->R<Value>{
 use jni::objects::{JObject,JValue};
 let number=call_number(number)?;
 let permission=env.new_string("android.permission.CALL_PHONE").map_err(|_|"Permission check failed.")?;
 let granted=env.call_method(context,"checkSelfPermission","(Ljava/lang/String;)I",&[JValue::Object(permission.as_ref())]).and_then(|v|v.i()).map_err(|_|"Permission check failed.")?==0;
 if !granted{return Ok(json!({"status":"needs_permission","number":number,"message":"Enable phone calls in Secretary on the S20 FE."}))}
 if dry_run{return Ok(json!({"status":"ready","number":number,"placed":false}))}
 let service=env.new_string("telecom").map_err(|_|"Telecom unavailable.")?;
 let telecom=env.call_method(context,"getSystemService","(Ljava/lang/String;)Ljava/lang/Object;",&[JValue::Object(service.as_ref())]).and_then(|v|v.l()).map_err(|_|"Telecom unavailable.")?;
 let scheme=env.new_string("tel").map_err(|_|"Invalid phone URI.")?;
 let value=env.new_string(&number).map_err(|_|"Invalid phone URI.")?;
 let uri=env.call_static_method("android/net/Uri","fromParts","(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)Landroid/net/Uri;",&[JValue::Object(scheme.as_ref()),JValue::Object(value.as_ref()),JValue::Object(&JObject::null())]).and_then(|v|v.l()).map_err(|_|"Invalid phone URI.")?;
 let extras=env.new_object("android/os/Bundle","()V",&[]).map_err(|_|"Telecom request failed.")?;
 if env.call_method(&telecom,"placeCall","(Landroid/net/Uri;Landroid/os/Bundle;)V",&[JValue::Object(&uri),JValue::Object(&extras)]).is_err(){let _=env.exception_clear();return Err("Android refused the call. Check the SIM and phone permission.".into())}
 Ok(json!({"status":"submitted","number":number,"placed":true,"message":"Android Telecom accepted the call request; connection is not yet confirmed."}))
}

#[cfg(feature="android-network")]
#[no_mangle]
pub extern "system" fn Java_tech_getsecretary_secretary_NativeCalls_phoneCall(mut env:jni::JNIEnv,_class:jni::objects::JClass,context:jni::objects::JObject,number:jni::objects::JString,dry_run:jni::sys::jboolean)->jni::sys::jstring{
 let result=std::panic::catch_unwind(std::panic::AssertUnwindSafe(||->R<Value>{
  let number:String=env.get_string(&number).map_err(|_|"Invalid number.")?.into();
  phone_call(&mut env,&context,&number,dry_run!=0)
 }));
 let value=match result{Ok(Ok(v))=>v,Ok(Err(e))=>json!({"status":"error","message":e}),Err(_)=>json!({"status":"error","message":"Phone function failed."})};
 env.new_string(value.to_string()).map(|v|v.into_raw()).unwrap_or(std::ptr::null_mut())
}
#[cfg(test)]mod phone_call_tests{
 use super::*;
 #[test]fn exact_owner_number_and_no_commands(){assert_eq!(call_number("+45 66 77 87 02").unwrap(),"+4566778702");for s in ["tel:+4566778702","*21*12345#","+45;reboot","$(touch x)","++4512345678",""]{assert!(call_number(s).is_err(),"{s}");}}
}
