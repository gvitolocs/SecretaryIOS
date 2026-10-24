pub mod calls;
use regex::Regex;
use std::sync::LazyLock;
static CONTEXT:LazyLock<Regex>=LazyLock::new(||Regex::new(r"(?iu)(?:\b(?:otp|one[ -]time|verification|verify|security|authentication|authentification|login|sign[ -]?in|access|verifica|sicurezza|accesso|autenticazione|bestätigung|bestatigung|anmelde|sicherheits|bekræft|bekraeft|engangs|verifikations|vérification|connexion|sécurité|seguridad|verificación|autenticación|confirmação|подтверждения|проверки|входа)\p{L}*|验证码|驗證碼|認証コード|인증|(?:your|votre|tuo|tu|din|dit|dein)\s+(?:code|codice|código|codigo|kode)\b)").unwrap());
static CODES:LazyLock<Regex>=LazyLock::new(||Regex::new(r"\b(?:[0-9]{3}[- ][0-9]{3}|[0-9]{4,8}|[A-Z0-9]{4,10})\b").unwrap());
static GOOGLE:LazyLock<Regex>=LazyLock::new(||Regex::new(r"(?i)\bG[-\s\x{2011}\x{2013}]([0-9]{6})\b").unwrap());
static URL:LazyLock<Regex>=LazyLock::new(||Regex::new(r"https?://\S+").unwrap());
pub fn parse_otp(sender:&str,body:&str)->Option<String>{
 if body.len()>8192{return None}
 let normalized:String=body.chars().map(|c| {
  // Common Unicode decimal digit blocks; other text is preserved.
  let n=c as u32;let zero=[0x660,0x6f0,0x966,0xff10].into_iter().find(|z|n>=*z&&n<*z+10);
  zero.map_or(c,|z|char::from_u32(48+n-z).unwrap())
 }).collect();
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
 #[test]fn requests_only(){assert_eq!(otp_intent("dimmi il codice di verifica"),Some("take"));assert_eq!(otp_intent("implement an OTP service"),None);}
}
