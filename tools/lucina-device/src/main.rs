mod phone;mod otp;mod dashboard;
use serde_json::{json,Value};use std::io::{self,Read};
type R<T>=Result<T,String>;
fn input()->R<Value>{let mut s=String::new();io::stdin().take(65537).read_to_string(&mut s).map_err(|_|"Input unavailable")?;if s.len()>65536{return Err("Input too large".into())}serde_json::from_str(&s).map_err(|_|"Invalid JSON".into())}
fn run()->R<Value>{
 match std::env::args().nth(1).as_deref(){
  Some("mcp")=>{phone::mcp()?;Ok(Value::Null)},
  Some("otp-server")=>{otp::serve()?;Ok(Value::Null)},
  Some("otp-intent")=>{let v=input()?;Ok(json!({"intent":lucina_device::otp_intent(v["message"].as_str().unwrap_or(""))}))},
  Some("otp-chat")=>otp::chat(input()?),
  Some("dashboard")=>dashboard::run(input()?),
  _=>Err("Use mcp, otp-server, otp-intent, otp-chat or dashboard.".into())
 }
}
fn main(){match run(){Ok(v)if !v.is_null()=>println!("{}",json!({"ok":true,"data":v})),Ok(_)=>{},Err(_)=>{println!("{}",json!({"ok":false,"error":"Device operation unavailable or invalid."}));std::process::exit(1)}}}
