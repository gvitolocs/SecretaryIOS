use crate::R;use serde_json::{json,Value};use std::{fs,io::Write,os::unix::fs::OpenOptionsExt};use fs2::FileExt;
const PROFILE:&str="/home/nez/Projects/Lucina/data/hermes-cua-profile";
fn parse(raw:&str)->R<Value>{serde_yaml::from_str(raw).map_err(|_|"Hermes configuration could not be parsed.".into())}
fn enabled(c:&Value)->bool{c["plugins"]["enabled"].as_array().is_some_and(|a|a.iter().any(|v|v=="advanced-delegation"))&&std::path::Path::new(PROFILE).join("plugins/advanced-delegation").is_dir()}
fn rows(c:&Value)->Vec<Value>{c["delegation"]["backends"]["model_map"]["options"].as_array().cloned().unwrap_or_default()}
fn plugins(c:&Value)->Vec<Value>{
 let entries=[
 ("advanced-delegation","Advanced Delegation","Model and profile routing"),
 ("agent-importers","Agent Importers","Import runs only when explicitly requested; credentials are not imported"),
 ("mcp-serve","MCP Serve","Available locally; no public server started"),
 ("memory-wiki","Memory Wiki","Local Hermes history; native Hermes dashboard extension"),
 ("langfuse","Langfuse","Needs a Langfuse endpoint and keys; tracing is off"),
 ("claude-subscription-directsdk-experimental","Claude Subscription DirectSDK","Needs an authenticated Claude CLI; existing Gemini routing stays active"),
 ("meshy","Meshy","Inactive: upstream requires Node.js and a Meshy account"),
 ("blender","Blender","Needs Blender 5.1+ and its MCP add-on"),
 ("td","TouchDesigner","Needs a running TouchDesigner session on macOS or Windows"),
 ("spotify","Spotify","Needs Spotify app credentials and account login"),
 ("homeassistant","Home Assistant","Needs a Home Assistant endpoint and token"),
 ("xchat","X Chat","Needs an X developer app and Chat API login"),
 ("snyk","Snyk","Inactive: upstream MCP server requires Node.js and account login"),
 ("rea","REA","Inactive: upstream MCP server requires Node.js"),
 ("camofox","Camofox","Needs a running Camofox server; Lucina currently uses her Cua Firefox"),
 ("bubblewrap","Bubblewrap","Alternative terminal backend; Cua remains selected"),
 ("singularity","Singularity / Apptainer","Alternative backend; needs Apptainer; Cua remains selected"),
 ("accent-picker","Accent Picker","Native Hermes Desktop theme extension")];
 entries.iter().map(|(id,label,note)|json!({"id":id,"label":label,"installed":std::path::Path::new(PROFILE).join("plugins").join(id).is_dir(),"enabled":c["plugins"]["enabled"].as_array().is_some_and(|a|a.iter().any(|v|v==id)),"note":note})).collect()
}
fn view(c:&Value)->Value{
 let plugin=enabled(c);let models=rows(c);let profiles=c["delegation"]["backends"]["profile_routes"]["profiles"].as_array().cloned().unwrap_or_default();
 let allowed=["name","description","model","provider","reasoning_effort","service_tier"];
 let clean:Vec<Value>=models.iter().map(|row|{let mut o=serde_json::Map::new();for k in allowed{if let Some(v)=row.get(k){o.insert(k.into(),v.clone());}}Value::Object(o)}).collect();
 let profiles:Vec<Value>=profiles.iter().map(|v|json!({"name":v["name"],"description":v["description"]})).collect();
 json!({"mode":c["delegation"]["backend"].as_str().unwrap_or("default"),"pluginInstalled":plugin,"model":{"provider":c["model"]["provider"],"default":c["model"]["default"]},
 "backends":[{"name":"default","label":"Default","available":true,"reason":""},
 {"name":"model_map","label":"Model routes","available":plugin&&!models.is_empty(),"reason":if plugin&&models.is_empty(){"Add a model route first."}else if !plugin{"Official plugin unavailable."}else{""}},
 {"name":"profile_routes","label":"Profile routes","available":plugin&&!profiles.is_empty(),"reason":if profiles.is_empty(){"No isolated profiles configured."}else if !plugin{"Official plugin unavailable."}else{""}}],
 "modelRoutes":clean,"defaultRoute":c["delegation"]["backends"]["model_map"]["default"],"profileRoutes":profiles,"availableProfiles":[],"plugins":plugins(c),"appliesTo":"new sessions","sandbox":"lucina-workspace","androidSandbox":"lucina-android","runtime":"Rust"})
}
fn replace_delegation(raw:&str,delegation:&Value)->R<String>{
 let yaml=serde_yaml::to_string(&json!({"delegation":delegation})).map_err(|_|"Could not serialize routing.")?;
 let mut out=String::new();let mut skipping=false;let mut replaced=false;
 for line in raw.split_inclusive('\n'){
  if line.starts_with("delegation:"){if !replaced{out.push_str(&yaml);replaced=true;}skipping=true;continue}
  let top=!line.starts_with([' ','\t','#','\r','\n'])&&!line.trim().is_empty();
  if skipping&&top{skipping=false}
  if !skipping{out.push_str(line)}
 }
 if !replaced{if !out.ends_with('\n'){out.push('\n')}out.push_str(&yaml)}
 Ok(out)
}
pub fn run(v:Value)->R<Value>{
 let path=std::path::Path::new(PROFILE).join("config.yaml");
 if fs::metadata(&path).map_err(|_|"Hermes configuration unavailable.")?.len()>256*1024{return Err("Hermes configuration exceeds limit.".into())}
 let action=v["action"].as_str().unwrap_or("view");
 let lock=fs::OpenOptions::new().create(true).read(true).write(true).mode(0o600).open(std::path::Path::new(PROFILE).join(".secretary-routing.lock")).map_err(|_|"Configuration lock unavailable.")?;
 lock.lock_exclusive().map_err(|_|"Configuration busy.")?;
 let raw=fs::read_to_string(&path).map_err(|_|"Hermes configuration unavailable.")?;let mut c=parse(&raw)?;
 if action=="mode"{
  let mode=v["mode"].as_str().ok_or("Missing mode.")?;
  if !["default","model_map","profile_routes"].contains(&mode){return Err("Unknown delegation mode.".into())}
  let status=view(&c);if !status["backends"].as_array().unwrap().iter().any(|b|b["name"]==mode&&b["available"]==true){return Err("This delegation mode is unavailable.".into())}
  c["delegation"]["backend"]=json!(mode);
 }else if action=="models"{
  if !enabled(&c){return Err("Official advanced-delegation plugin unavailable.".into())}
  let rows=v["routes"].as_array().filter(|r|r.len()<=8).ok_or("At most eight routes.")?;let mut names=std::collections::HashSet::new();let mut clean=Vec::new();
  let name_re=regex::Regex::new(r"^[a-z][a-z0-9_-]{0,31}$").unwrap();let model_re=regex::Regex::new(r"^gemini-[a-zA-Z0-9.-]{1,80}$").unwrap();
  for row in rows{
   let name=row["name"].as_str().filter(|s|name_re.is_match(s)).ok_or("Invalid route name.")?;
   let desc=row["description"].as_str().filter(|s|!s.trim().is_empty()&&s.len()<=200).ok_or("Each route needs a short description.")?;
   let model=row["model"].as_str().filter(|s|model_re.is_match(s)).ok_or("Use a Gemini model ID with the existing provider.")?;
   if !names.insert(name){return Err("Route names must be unique.".into())}
   clean.push(json!({"name":name,"description":desc,"provider":"gemini","model":model}));
  }
  let default=v["default"].as_str().filter(|s|!s.is_empty());
  if default.is_some_and(|s|!names.contains(s)){return Err("Default route must be in the route list.".into())}
  c["delegation"]["backends"]["model_map"]=json!({"options":clean,"default":default});
  // Clearing an active route list safely restores the built-in mode.
  if clean.is_empty()&&c["delegation"]["backend"]=="model_map"{c["delegation"]["backend"]=json!("default")}
 }else if action!="view"{return Err("Unsupported Hermes operation.".into())}
 if action!="view"{
  let updated=replace_delegation(&raw,&c["delegation"])?;
  parse(&updated)?;
  if fs::read_to_string(&path).map_err(|_|"Configuration changed.")?!=raw{return Err("Configuration changed; refresh and retry.".into())}
  let temp=path.with_extension(format!("yaml.{}.tmp",std::process::id()));
  let mut file=fs::OpenOptions::new().create_new(true).write(true).mode(0o600).open(&temp).map_err(|_|"Configuration write unavailable.")?;
  file.write_all(updated.as_bytes()).and_then(|_|file.sync_all()).map_err(|_|"Configuration write failed.")?;
  fs::rename(&temp,&path).map_err(|_|"Configuration save failed.")?;
 }
 Ok(view(&c))
}
#[cfg(test)]mod tests{
 use super::*;
 #[test]fn changes_only_routing_and_preserves_unrelated_comments(){let raw="# preserved\ntools:\n  tool_search: off\n# retained\nmodel:\n  default: gemini-test\ndelegation:\n  backend: default\nterminal:\n  backend: cua\n";let s=replace_delegation(raw,&json!({"backend":"model_map"})).unwrap();assert!(s.starts_with("# preserved\ntools:"));assert!(s.contains("# retained\nmodel:"));assert!(s.contains("terminal:\n  backend: cua"));assert_eq!(parse(&s).unwrap()["delegation"]["backend"],"model_map");}
 #[test]fn dashboard_filters_secrets(){let v=view(&json!({"model":{"provider":"gemini","default":"gemini-test","api_key":"private-secret"},"delegation":{"backends":{"model_map":{"options":[{"name":"fast","api_key":"private-secret","base_url":"https://secret.test/key"}]}}}}));assert!(!v.to_string().contains("private-secret"));assert!(!v.to_string().contains("secret.test"));}
}
