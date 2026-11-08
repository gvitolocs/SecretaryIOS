use crate::R;use serde_json::{json,Value};use std::{collections::{VecDeque,HashMap},fs,io::Read,sync::Arc,time::{Duration,SystemTime,UNIX_EPOCH,Instant}};
pub fn now()->u64{SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64}
fn agent()->ureq::Agent{ureq::Agent::config_builder().tls_config(ureq::tls::TlsConfig::builder().unversioned_rustls_crypto_provider(Arc::new(rustls_graviola::default_provider())).build()).timeout_global(Some(Duration::from_secs(5))).max_redirects(0).build().new_agent()}
pub fn remote(action:&str,seconds:u64)->R<Value>{
 if !["status","arm","take","disarm"].contains(&action){return Err("Unknown OTP action.".into())}
 let token=fs::read_to_string("/home/nez/Projects/Lucina/data/mobile-otp/internet-owner.token").map_err(|_|"Internet OTP pairing is unavailable.")?;
 let route=if action=="take"{"otp"}else{action};
 let url=format!("https://getsecretary.tech/v1/phone/otp/{route}");
 let a=agent();let auth=format!("Bearer {}",token.trim());
 let mut response=if ["arm","disarm"].contains(&action){
  a.post(&url).header("Authorization",&auth).send_json(json!({"seconds":seconds.clamp(30,300)}))
 }else{a.get(&url).header("Authorization",&auth).call()}.map_err(|_|"Internet OTP bridge unavailable.")?;
 if response.status().as_u16()==204{return Ok(json!({"waiting":true}))}
 let bytes=response.body_mut().with_config().limit(8192).read_to_vec().map_err(|_|"Invalid OTP response.")?;
 serde_json::from_slice(&bytes).map_err(|_|"Invalid OTP response.".into())
}
pub fn private_owner(v:&Value)->bool{
 if v["surface"]=="app"{return v["ownerAuthorized"]==true&&v["mcp"]!=true}
 let owner=v["ownerId"].as_str().unwrap_or("");
 !owner.is_empty()&&owner.bytes().all(|b|b.is_ascii_digit())&&v["chatType"]=="private"&&v["userId"]==owner&&v["chatId"]==owner&&v["isBot"]!=true
}
fn answer(reply:&str)->Value{json!({"handled":true,"reply":reply,"sensitive":true})}
pub fn chat(v:Value)->R<Value>{
 let Some(action)=lucina_device::otp_intent(v["message"].as_str().unwrap_or("")) else{return Ok(json!({"handled":false}))};
 if !private_owner(&v){return Ok(answer("OTP access requires Giuseppe’s own private app or Telegram chat."))}
 let result=(||->R<Value>{
  match action{
   "disarm"=>{remote("disarm",0)?;Ok(answer("OTP listening stopped; pending codes cleared."))},
   "status"=>{let s=remote("status",0)?;Ok(answer(if s["armed"]==true{"The internet OTP bridge is ready and listening."}else{"The internet OTP bridge is ready. Listening is off."}))},
   "arm"=>{let s=remote("arm",300)?;Ok(answer(if s["continuous"]==true{"Background OTP forwarding is enabled for new codes from any provider. Ask me here when you need a code; each code expires after two minutes."}else{"I’m listening for new OTPs from any provider for 5 minutes. Request a fresh code, then ask me for your OTP here."}))},
   _=>{
    let d=remote("take",0)?;
    if d["waiting"]==true{
     let mut s=remote("status",0)?;if s["armed"]!=true{s=remote("arm",300)?;}
     if s["continuous"]==true{return Ok(answer("No fresh OTP is waiting. Background forwarding is enabled. Ask me here within two minutes of receiving a new code. I cannot read SMS history."))}
     return Ok(answer("No fresh OTP is waiting. I’m listening for new codes over the internet for up to 5 minutes. Request one from the service, then ask me for your OTP here. I cannot read earlier SMS."))
    }
    let code=d["code"].as_str().filter(|s|s.len()>=4&&s.len()<=10&&s.bytes().all(|b|b.is_ascii_alphanumeric())).ok_or("Invalid OTP.")?;
    let expires=d["expiresAt"].as_u64().filter(|t|*t>now()).ok_or("Expired OTP.")?;
    let sender:String=d["sender"].as_str().unwrap_or("SMS").chars().filter(|c|!c.is_control()&&*c!='<'&&*c!='>').take(80).collect();
    Ok(json!({"handled":true,"reply":format!("OTP from {sender}: {code}\nCheck it matches the service you requested. Delivered once; expires shortly."),"sensitive":true,"expiresAt":expires}))
   }
  }
 })();
 Ok(result.unwrap_or_else(|_|answer("The internet OTP bridge is unavailable. Check that the S20 FE has internet and Lucina OTP is enabled.")))
}
struct Window {id:String,end:Option<Instant>,expires:u64}
struct Entry {data:Value,end:Instant}
#[derive(Default)]
struct Store{window:Option<Window>,pending:VecDeque<Entry>,seen:HashMap<String,Instant>}
impl Store{
 fn armed(&self)->bool{self.window.as_ref().is_some_and(|w|w.end.is_none_or(|end|Instant::now()<end))}
 fn continuous(&mut self)->Value{
  self.arm(300);
  if let Some(w)=self.window.as_mut(){w.end=None;w.expires=u64::MAX;}
  self.window()
 }
 fn arm(&mut self,seconds:u64)->Value{
  self.clear();let seconds=seconds.clamp(30,300);let mut random=[0u8;24];fs::File::open("/dev/urandom").unwrap().read_exact(&mut random).unwrap();
  let id:String=random.iter().map(|b|format!("{b:02x}")).collect();let expires=now()+seconds*1000;
  self.window=Some(Window{id:id.clone(),end:Some(Instant::now()+Duration::from_secs(seconds)),expires});
  json!({"armed":true,"windowId":id,"expiresAt":expires})
 }
 fn clear(&mut self){self.window=None;self.pending.clear();self.seen.clear();}
 fn window(&self)->Value{match self.window.as_ref().filter(|_|self.armed()){Some(w)=>json!({"armed":true,"windowId":w.id,"expiresAt":if w.end.is_none(){None}else{Some(w.expires)},"continuous":w.end.is_none()}),None=>json!({"armed":false,"continuous":false})}}
 fn receive(&mut self,v:&Value)->R<Value>{
  let w=self.window.as_ref().filter(|_|self.armed()).ok_or("No active window.")?;
  if v["windowId"].as_str()!=Some(w.id.as_str()){return Err("Wrong verification window.".into())}
  let code=v["code"].as_str().filter(|s|s.len()>=4&&s.len()<=10&&s.bytes().all(|b|b.is_ascii_alphanumeric())).ok_or("Invalid OTP.")?;
  let id=v["eventId"].as_str().filter(|s|s.len()>=16&&s.len()<=80&&s.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'-')).ok_or("Invalid SMS event.")?;
  self.seen.retain(|_,end|Instant::now()<*end);self.pending.retain(|e|Instant::now()<e.end);
  if self.seen.contains_key(id){return Ok(json!({"accepted":true,"duplicate":true}))}
  if self.seen.len()>=64{return Err("Verification event limit.".into())}
  self.seen.insert(id.into(),Instant::now()+Duration::from_secs(120));let expires=(now()+120000).min(w.expires);
  let sender:String=v["sender"].as_str().unwrap_or("SMS").chars().filter(|c|!c.is_control()).take(80).collect();
  let data=json!({"sender":sender,"code":code,"receivedAt":now(),"expiresAt":expires});
  if self.pending.len()>=8{self.pending.pop_front();}
  self.pending.push_back(Entry{data,end:Instant::now()+Duration::from_millis(expires.saturating_sub(now()))});
  Ok(json!({"accepted":true}))
 }
 fn take(&mut self)->Option<Value>{if !self.armed(){self.clear();return None}while let Some(e)=self.pending.pop_back(){if Instant::now()<e.end{return Some(e.data)}}None}
}
fn equal(a:&str,b:&str)->bool{a.len()==b.len()&&a.bytes().zip(b.bytes()).fold(0u8,|x,(a,b)|x|(a^b))==0}
pub fn serve()->R<()>{
 let dir=std::env::var("LUCINA_OTP_STATE_DIR").unwrap_or("/var/lib/lucina-otp".into());
 let owner=fs::read_to_string(format!("{dir}/owner.token")).map_err(|_|"Owner pairing missing.")?;
 let device=fs::read_to_string(format!("{dir}/phone.token")).map_err(|_|"Phone pairing missing.")?;
 if owner.trim().len()<40||device.trim().len()<40||equal(owner.trim(),device.trim()){return Err("Invalid distinct pairing credentials.".into())}
 let server=tiny_http::Server::http("127.0.0.1:18866").map_err(|_|"OTP listener unavailable.")?;
 let continuous=std::env::var("LUCINA_OTP_CONTINUOUS").as_deref()==Ok("true");
 let paused_path=format!("{dir}/paused");
 let mut store=Store::default();
 if continuous&&!std::path::Path::new(&paused_path).exists(){store.continuous();}
 for mut req in server.incoming_requests(){
  let path=req.url().split('?').next().unwrap_or("").to_string();
  let auth=req.headers().iter().find(|h|h.field.equiv("Authorization")).map(|h|h.value.as_str()).unwrap_or("");
  let phone=path=="/window"||path=="/incoming";let expected=format!("Bearer {}",if phone{device.trim()}else{owner.trim()});
  let mut status=200;
  let response=if !equal(auth,&expected){status=401;json!({"error":"unauthorized"})}else{
   let method=req.method().as_str().to_owned();let mut bytes=Vec::new();
   if req.body_length().is_some_and(|n|n>2048){status=413;json!({"error":"body_limit"})}
   else if req.as_reader().take(2049).read_to_end(&mut bytes).is_err()||bytes.len()>2048{status=400;json!({"error":"invalid_request"})}
   else{
    let body=if bytes.is_empty(){Ok(json!({}))}else{serde_json::from_slice::<Value>(&bytes)};
    if let Ok(v)=body{
     match (method.as_str(),path.as_str()){
      ("GET","/status")=>json!({"version":"0.4.0","transport":"https-event-driven","armed":store.armed(),"continuous":store.window()["continuous"]}),
      ("GET","/window")=>store.window(),
      ("POST","/arm")=>{if continuous{let _=fs::remove_file(&paused_path);store.continuous()}else{store.arm(v["seconds"].as_u64().unwrap_or(180))}},
      ("POST","/disarm")=>{if continuous&&fs::write(&paused_path,b"paused\n").is_err(){status=503;json!({"error":"pause_unavailable"})}else{store.clear();json!({"disarmed":true})}},
      ("GET","/otp")=>match store.take(){Some(v)=>v,None=>{status=204;Value::Null}},
      ("POST","/incoming")=>store.receive(&v).unwrap_or_else(|_|{status=409;json!({"error":"inactive_or_invalid_window"})}),
      _=>{status=404;json!({"error":"not_found"})}
     }
    }else{status=400;json!({"error":"invalid_json"})}
   }
  };
  let body=if status==204{String::new()}else{response.to_string()};
  let mut response=tiny_http::Response::from_string(body).with_status_code(status);
  response.add_header(tiny_http::Header::from_bytes("Content-Type","application/json").unwrap());
  response.add_header(tiny_http::Header::from_bytes("Cache-Control","no-store").unwrap());
  let _=req.respond(response);
 }
 Ok(())
}
#[cfg(test)]mod tests{
 use super::*;
 #[test]fn ttl_window_nonce_replay_and_consume_once(){let mut s=Store::default();let w=s.arm(60);let v=json!({"windowId":w["windowId"],"eventId":"0123456789abcdef","code":"482917","sender":"Bank"});assert!(s.receive(&v).is_ok());assert_eq!(s.receive(&v).unwrap()["duplicate"],true);assert_eq!(s.take().unwrap()["code"],"482917");assert!(s.take().is_none());s.clear();assert!(s.receive(&v).is_err());assert!(s.take().is_none());}
 #[test]fn continuous_window_keeps_short_code_lifetime(){let mut s=Store::default();let w=s.continuous();assert_eq!(w["continuous"],true);assert!(w["expiresAt"].is_null());let v=json!({"windowId":w["windowId"],"eventId":"0123456789abcdef","code":"482917"});s.receive(&v).unwrap();let code=s.take().unwrap();assert!(code["expiresAt"].as_u64().unwrap()<=now()+120000);s.seen.insert("old-event-0123456789".into(),Instant::now()-Duration::from_secs(1));let next=json!({"windowId":w["windowId"],"eventId":"0123456789abcdef-next","code":"482918"});s.receive(&next).unwrap();assert!(!s.seen.contains_key("old-event-0123456789"));s.clear();assert!(!s.armed());assert!(s.receive(&next).is_err());}
 #[test]fn private_owner_only(){assert!(private_owner(&json!({"surface":"telegram","ownerId":"42","userId":"42","chatId":"42","chatType":"private"})));assert!(!private_owner(&json!({"surface":"telegram","ownerId":"42","userId":"42","chatId":"-42","chatType":"group"})));assert!(!private_owner(&json!({"surface":"app","ownerAuthorized":true,"mcp":true})));}
}
