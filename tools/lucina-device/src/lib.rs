pub mod calls;
use regex::Regex;
use std::sync::LazyLock;
static CONTEXT:LazyLock<Regex>=LazyLock::new(||Regex::new(r"(?iu)(?:\b(?:otp|one[ -]time|verification|verify|security|authentication|authentification|login|sign[ -]?in|access|verifica|sicurezza|accesso|autenticazione|bestätigung|bestatigung|anmelde|sicherheits|bekræft|bekraeft|engangs|verifikations|vérification|connexion|sécurité|seguridad|verificación|autenticación|confirmação|подтверждения|проверки|входа)\p{L}*|验证码|驗證碼|認証コード|인증|(?:your|votre|tuo|tu|din|dit|dein)\s+(?:code|codice|código|codigo|kode)\b)").unwrap());
static CODES:LazyLock<Regex>=LazyLock::new(||Regex::new(r"\b(?:[0-9]{3}[- ][0-9]{3}|[0-9]{4,8}|[A-Z0-9]{4,10})\b").unwrap());
static GOOGLE:LazyLock<Regex>=LazyLock::new(||Regex::new(r"(?i)\bG[-\s\x{2011}\x{2013}]([0-9]{6})\b").unwrap());
static URL:LazyLock<Regex>=LazyLock::new(||Regex::new(r"https?://\S+").unwrap());
pub fn parse_otp(sender:&str,body:&str)->Option<String>{
 if body.len()>8192{return None}
 let normalized:String=body.chars().map(|c|unicode_digit(c).unwrap_or(c)).collect();
 let text=URL.replace_all(&normalized,"");
 if sender.eq_ignore_ascii_case("google")||text.to_lowercase().contains("google"){
  if let Some(m)=GOOGLE.captures(&text){return Some(m[1].to_owned())}
 }
 let mut best=65;let mut result=None;
 for context in CONTEXT.find_iter(&text){for m in CODES.find_iter(&text){
  let code=m.as_str().replace(['-',' '],"");
  if !code.bytes().any(|b|b.is_ascii_digit())||code.bytes().all(|b|b.is_ascii_digit())&&code.len()>8{continue}
  // Exclude phone numbers and codes embedded in a longer identifier.
  if text[..m.start()].ends_with('+'){continue}
  let distance=if m.end()<=context.start(){context.start()-m.end()}else{m.start().saturating_sub(context.end())};
  if distance<best{best=distance;result=Some(code)}
 }}
 result
}
/// Common Unicode decimal digit blocks (Arabic-Indic, Extended Arabic-Indic, Devanagari, full-width) as ASCII.
fn unicode_digit(c:char)->Option<char>{
 let n=c as u32;[0x660,0x6f0,0x966,0xff10].into_iter().find(|z|n>=*z&&n<*z+10).and_then(|z|char::from_u32(48+n-z))
}
/// Maximum distance, in bytes, between a code and its context word (as in parse_otp).
const MAX_DISTANCE:usize=64;
/// Replacement for a redacted code.
pub const REDACTED:&str="[otp]";
/// `text` with every OTP-looking code near an OTP context word (and every Google `G-123456`) replaced by
/// [`REDACTED`]. Copied from lucina-phone-core's `redact_otps`, for text that may reach a model (all
/// phone_* tool output). It errs towards redacting: any code-shaped token within MAX_DISTANCE bytes of a
/// context word goes. Unicode digits are matched through an ASCII copy whose spans map back to the
/// original, so redaction never changes other text.
pub fn redact_otps(text:&str)->std::borrow::Cow<'_,str>{
 use std::borrow::Cow;
 let (norm,map):(Cow<str>,Option<Vec<usize>>)=if text.chars().any(|c|unicode_digit(c).is_some()){
  let mut s=String::with_capacity(text.len());let mut m=Vec::with_capacity(text.len()+1);
  for (i,c) in text.char_indices(){let d=unicode_digit(c).unwrap_or(c);m.extend(std::iter::repeat_n(i,d.len_utf8()));s.push(d)}
  m.push(text.len());(Cow::Owned(s),Some(m))
 }else{(Cow::Borrowed(text),None)};
 let norm=norm.as_ref();
 // Context matches do not overlap and are sorted, so the nearest one to a code is the last one that
 // starts before the code ends, or the first one after it.
 let contexts:Vec<(usize,usize)>=CONTEXT.find_iter(norm).map(|m|(m.start(),m.end())).collect();
 let mut spans:Vec<(usize,usize)>=GOOGLE.find_iter(norm).map(|m|(m.start(),m.end())).collect();
 if !contexts.is_empty(){for m in CODES.find_iter(norm){
  let code=m.as_str().replace(['-',' '],"");
  if !code.bytes().any(|b|b.is_ascii_digit())||code.bytes().all(|b|b.is_ascii_digit())&&code.len()>8||norm[..m.start()].ends_with('+'){continue}
  let i=contexts.partition_point(|&(start,_)|start<m.end());
  let after=contexts.get(i).map(|&(start,_)|start-m.end());
  let before=i.checked_sub(1).map(|j|m.start().saturating_sub(contexts[j].1));
  if after.into_iter().chain(before).any(|d|d<=MAX_DISTANCE){spans.push((m.start(),m.end()))}
 }}
 if spans.is_empty(){return Cow::Borrowed(text)}
 let orig=|i:usize|map.as_ref().map_or(i,|m|m[i]);
 spans.sort_unstable();
 let mut out=String::with_capacity(text.len());let mut at=0;
 for (start,end) in spans{
  let (start,end)=(orig(start),orig(end));
  if start<at{at=at.max(end);continue}
  out.push_str(&text[at..start]);out.push_str(REDACTED);at=end;
 }
 out.push_str(&text[at..]);
 Cow::Owned(out)
}
pub fn otp_intent(text:&str)->Option<&'static str>{
 let s=text.trim().to_lowercase();
 match s.as_str(){"/otp"=>return Some("take"),"/otp arm"=>return Some("arm"),"/otp stop"=>return Some("disarm"),"/otp status"=>return Some("status"),_=>{}}
 static SUBJECT:LazyLock<Regex>=LazyLock::new(||Regex::new(r"(?iu)\b(?:otp|verification codes?|security codes?|sms codes?|codic[ei] (?:di (?:verifica|sicurezza|accesso)|sms)|codice otp|verifikationskode|engangskode)\b").unwrap());
 static ASK:LazyLock<Regex>=LazyLock::new(||Regex::new(r"(?iu)\b(?:read|show|tell|send|get|give|check|what|latest|leggi|leggimi|mostra|mostrami|dimmi|mandami|invia|ultimo|qual|læs|vis)\b").unwrap());
 (SUBJECT.is_match(&s)&&ASK.is_match(&s)).then_some("take")
}
#[cfg(feature="android")]
#[no_mangle]
pub extern "system" fn Java_tech_getsecretary_lucinaotp_NativeOtp_parse(mut env:jni::JNIEnv,_class:jni::objects::JClass,sender:jni::objects::JString,body:jni::objects::JString)->jni::sys::jstring{
 let result=std::panic::catch_unwind(std::panic::AssertUnwindSafe(||{
  let s:String=env.get_string(&sender).ok()?.into();let b:String=env.get_string(&body).ok()?.into();
  parse_otp(&s,&b).and_then(|code|env.new_string(code).ok()).map(|s|s.into_raw())
 }));
 result.ok().flatten().unwrap_or(std::ptr::null_mut())
}
#[cfg(test)]
mod tests{
 use super::*;
 #[test]fn providers_languages_and_non_codes(){
  for (sender,body,want) in [
   ("Google","G-123456","123456"),("Bank","Your security code is 682149. Expires in 5 minutes.","682149"),
   ("Apple","Il tuo codice di verifica è 294728.","294728"),("Service","Код подтверждения: 294728.","294728"),
   ("Service","验证码：294728","294728"),("Service","Din engangskode er 294728.","294728"),
   ("Service","Your login code: X8H2K9.","X8H2K9"),
   ("Service","Votre code de sécurité est 294728.","294728"),
   ("Service","Ihr Sicherheitscode lautet 294728.","294728"),
   ("Service","Tu código de verificación es 294728.","294728"),
   ("Service","Your security code is 294-728.","294728"),("Service","Your verification code: ٢٩٤٧٢٨.","294728")
  ]{assert_eq!(parse_otp(sender,body).as_deref(),Some(want),"{body}")}
  for body in ["Invoice 123456, order code 9999.","Call +4512345678 at 12:30.","Your balance is 123456 euros.","Your verification code is at https://example.test/123456","G-1234567"]{assert_eq!(parse_otp("Shop",body),None,"{body}")}
 }
 #[test]fn redaction_hides_codes_and_keeps_other_text(){
  let text="Row: 0 address=Bank, body=Your security code is 682149. Order 123456 shipped yesterday to the depot in town.";
  let out=redact_otps(text);assert!(!out.contains("682149"),"{out}");assert!(out.contains("address=Bank"));
  assert_eq!(redact_otps("G-123456 is your Google verification code"),"[otp] is your Google verification code");
  assert_eq!(redact_otps("Il tuo codice di verifica è 294728."),"Il tuo codice di verifica è [otp].");
  assert_eq!(redact_otps("Your verification code: ٢٩٤٧٢٨. ok"),"Your verification code: [otp]. ok");
  assert_eq!(redact_otps("<node text=\"Your login code: X8H2K9\" resource-id=\"a\"/>"),"<node text=\"Your login code: [otp]\" resource-id=\"a\"/>");
  assert_eq!(redact_otps("482917 is your code"),"[otp] is your code");
  let plain="Battery 87%, build 20261024, nothing secret here.";
  assert!(matches!(redact_otps(plain),std::borrow::Cow::Borrowed(_)));
  assert_eq!(redact_otps("Call +4512345678 to verify"),"Call +4512345678 to verify");
  // A code far from any context word stays (bounded false positives).
  let far=format!("Your security code is coming. {} Order 123456.","x".repeat(80));assert_eq!(redact_otps(&far),far);
 }
 #[test]fn requests_only(){assert_eq!(otp_intent("dimmi il codice di verifica"),Some("take"));assert_eq!(otp_intent("implement an OTP service"),None);}
}
