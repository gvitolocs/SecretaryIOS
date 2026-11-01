//! Pure parsers over the text adb, pm, dumpsys and aapt2 print. No I/O here, so
//! every decision phonepkg takes on device output is unit-tested below.

use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::collections::BTreeMap;
use std::net::SocketAddr;

pub const WOLT: &str = "com.wolt.courierapp";

/// One line of `pm list packages [--show-versioncode] [-i]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PmPackage {
    pub package: String,
    pub version_code: Option<u64>,
    /// `None` when pm prints `installer=null` (sideloaded) or `-i` was not given.
    pub installer: Option<String>,
}

/// `package:com.foo versionCode:123  installer=com.android.vending` (tokens in any order,
/// any whitespace; `versionCode` and `installer` optional).
pub fn parse_pm_list(text: &str) -> Vec<PmPackage> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut tokens = line.split_whitespace();
        let Some(name) = tokens.next().and_then(|t| t.strip_prefix("package:")) else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        let mut pkg = PmPackage { package: name.to_string(), version_code: None, installer: None };
        for t in tokens {
            if let Some(v) = t.strip_prefix("versionCode:") {
                pkg.version_code = v.parse().ok();
            } else if let Some(v) = t.strip_prefix("installer=") {
                if !v.is_empty() && v != "null" {
                    pkg.installer = Some(v.to_string());
                }
            }
        }
        out.push(pkg);
    }
    out
}

/// pm's filter argument is a substring match; this picks the exact package.
pub fn find_exact<'a>(list: &'a [PmPackage], package: &str) -> Option<&'a PmPackage> {
    list.iter().find(|p| p.package == package)
}

/// Java package names only (`[A-Za-z][A-Za-z0-9_]*` segments joined by dots). Every name
/// that goes into a device shell command passes through this, so it is also the quoting guard.
pub fn is_valid_package_name(name: &str) -> bool {
    !name.is_empty()
        && name.split('.').all(|seg| {
            let mut chars = seg.chars();
            matches!(chars.next(), Some(c) if c.is_ascii_alphabetic())
                && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
}

/// The device-side loop whose output `parse_version_names` reads.
pub fn version_name_script(packages: &[&str]) -> Result<String> {
    for p in packages {
        if !is_valid_package_name(p) {
            bail!("not a package name: {p:?}");
        }
    }
    Ok(format!(
        "for p in {}; do echo \"@@$p\"; dumpsys package \"$p\" | grep -m1 versionName=; done",
        packages.join(" ")
    ))
}

/// Output of the loop: `@@<pkg>` then (maybe) `    versionName=X`. Packages with no
/// versionName line are absent from the map.
pub fn parse_version_names(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(pkg) = line.strip_prefix("@@") {
            current = Some(pkg.trim().to_string());
        } else if let (Some(pkg), Some(idx)) = (&current, line.find("versionName=")) {
            let value = line[idx + "versionName=".len()..].trim();
            out.entry(pkg.clone()).or_insert_with(|| value.to_string());
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Badging {
    pub package: String,
    pub version_code: u64,
    pub version_name: String,
}

/// `aapt2 dump badging`: the `package: name='x' versionCode='2' versionName='2.0' ...` line.
pub fn parse_badging(text: &str) -> Result<Badging> {
    let line = text
        .lines()
        .find_map(|l| l.strip_prefix("package:"))
        .context("aapt2 badging has no `package:` line")?;
    let attrs = quoted_attrs(line);
    let get = |key: &str| attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
    let package = get("name").filter(|n| !n.is_empty()).context("aapt2 badging has no package name")?;
    let code = get("versionCode").context("aapt2 badging has no versionCode")?;
    let version_code = code.parse().with_context(|| format!("bad versionCode {code:?}"))?;
    Ok(Badging { package, version_code, version_name: get("versionName").unwrap_or_default() })
}

/// `key='value' key2='value 2'` pairs.
fn quoted_attrs(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(eq) = rest.find("='") {
        let key = rest[..eq].split_whitespace().last().unwrap_or("").to_string();
        let after = &rest[eq + 2..];
        let Some(end) = after.find('\'') else { break };
        out.push((key, after[..end].to_string()));
        rest = &after[end + 1..];
    }
    out
}

/// `adb mdns services`: `<name>\t_adb-tls-connect._tcp\t<ip>:<port>` (tabs or spaces).
pub fn parse_mdns_tls(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        let Some(pos) = tokens.iter().position(|t| t.starts_with("_adb-tls-connect._tcp")) else {
            continue;
        };
        if let Some(addr) = tokens[pos + 1..].iter().find(|t| t.parse::<SocketAddr>().is_ok()) {
            if !out.iter().any(|a| a == addr) {
                out.push(addr.to_string());
            }
        }
    }
    out
}

/// `adb connect` exits 0 even when it fails; this reads its words.
pub fn connect_succeeded(output: &str) -> bool {
    let o = output.to_ascii_lowercase();
    o.contains("connected to") && !o.contains("failed") && !o.contains("cannot") && !o.contains("unable")
}

/// Host part of `host:port`.
pub fn target_host(target: &str) -> &str {
    target.rsplit_once(':').map_or(target, |(h, _)| h).trim_matches(|c| c == '[' || c == ']')
}

/// Port part of `host:port`.
pub fn target_port(target: &str) -> Result<u16> {
    let (_, port) = target.rsplit_once(':').with_context(|| format!("target {target:?} has no port"))?;
    port.parse().with_context(|| format!("target {target:?} has a bad port"))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Gate {
    pub busy: bool,
    pub reasons: Vec<String>,
}

/// The Wolt gate over `dumpsys activity activities`, `dumpsys activity services
/// com.wolt.courierapp` and `dumpsys location`. Empty text means the read failed: busy.
pub fn wolt_gate(activities: &str, services: &str, location: &str) -> Gate {
    let mut reasons = Vec::new();

    if activities.trim().is_empty() {
        reasons.push("could not read dumpsys activity activities".to_string());
    } else if activities
        .lines()
        .any(|l| (l.contains("topResumedActivity") || l.contains("mResumedActivity")) && l.contains(WOLT))
    {
        reasons.push("Wolt is the resumed (on-screen) activity".to_string());
    }

    if services.trim().is_empty() {
        reasons.push("could not read dumpsys activity services".to_string());
    } else if services.contains("isForeground=true") {
        reasons.push("Wolt has a foreground service".to_string());
    }

    if location.trim().is_empty() {
        reasons.push("could not read dumpsys location".to_string());
    } else {
        match location_lines_mentioning(location, WOLT) {
            None => reasons.push("could not read dumpsys location (no `Location Providers:` section)".to_string()),
            Some(lines) => {
                if let Some(first) = lines.first() {
                    reasons.push(format!("Wolt is requesting location: {first}"));
                }
            }
        }
    }

    Gate { busy: !reasons.is_empty(), reasons }
}

/// Lines of the `Location Providers:` section (up to `Historical Aggregate` / `Event Log:`)
/// that mention `package`, skipping the timestamped `[2026-…]` request history.
/// `None` if the section is missing.
fn location_lines_mentioning(text: &str, package: &str) -> Option<Vec<String>> {
    let mut lines = text.lines();
    lines.by_ref().find(|l| l.contains("Location Providers:"))?;
    let mut out = Vec::new();
    for line in lines {
        if line.contains("Historical Aggregate") || line.contains("Event Log:") {
            break;
        }
        let t = line.trim();
        let mut chars = t.chars();
        if chars.next() == Some('[') && chars.next().is_some_and(|c| c.is_ascii_digit()) {
            continue;
        }
        if t.contains(package) {
            out.push(t.to_string());
        }
    }
    Some(out)
}

/// Downgrades are refused, always: an installed versionCode above the APK's.
pub fn check_version(installed: Option<u64>, apk: u64) -> Result<()> {
    match installed {
        Some(inst) if inst > apk => {
            bail!("downgrade refused: installed versionCode {inst} > APK versionCode {apk}")
        }
        _ => Ok(()),
    }
}

/// Seconds since the Unix epoch as `YYYY-MM-DDTHH:MM:SSZ` (civil-from-days, H. Hinnant).
pub fn format_rfc3339(unix_secs: u64) -> String {
    let days = (unix_secs / 86_400) as i64;
    let rem = unix_secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCATION_IDLE: &str = "\
LOCATION MANAGER STATE (dumpsys location):
  Location Providers:
    passive provider:
      service: registered
      listeners:
        10228/com.google.android.gms[network_location_provider]/B75B7848 Request[PASSIVE, minUpdateInterval=0, hiddenFromAppOps, WorkSource{10228 com.google.android.gms}]
    gps provider:
      service: ProviderRequest[OFF]
      [2026-10-29 19:13:32]: ProviderRequest[@+5s0ms, HIGH_ACCURACY, WorkSource{10434 com.wolt.courierapp}] WorkSource{10434 com.wolt.courierapp},
  Historical Aggregate Location Provider Data:
      com.wolt.courierapp: min/max interval = +1s0ms/+5s0ms, total/active/foreground duration = +2h1m/+2h1m/+1h50m
  Event Log:
    10-29 19:13:32.101: ProviderRequest gps com.wolt.courierapp
";

    const ACTIVITIES_IDLE: &str = "\
ACTIVITY MANAGER ACTIVITIES (dumpsys activity activities)
Display #0 (activities from top to bottom):
  * Task{1a2b3c #12 type=home A=10087:com.sec.android.app.launcher}
    * ActivityRecord{77aa u0 com.sec.android.app.launcher/.activities.LauncherActivity t12}
  * Task{4d5e6f #1234 type=standard A=10434:com.wolt.courierapp}
    * ActivityRecord{88bb u0 com.wolt.courierapp/.MainActivity t1234}
    mResumedActivity: ActivityRecord{77aa u0 com.sec.android.app.launcher/.activities.LauncherActivity t12}
  topResumedActivity=ActivityRecord{77aa u0 com.sec.android.app.launcher/.activities.LauncherActivity t12}
";

    const SERVICES_IDLE: &str = "\
ACTIVITY MANAGER SERVICES (dumpsys activity services)
  User 0 active services:
  * ServiceRecord{cc11 u0 com.wolt.courierapp/.location.LocationService}
    intent={cmp=com.wolt.courierapp/.location.LocationService}
    isForeground=false foregroundId=0 foregroundNoti=null
";

    #[test]
    fn pm_list_with_installer_and_null() {
        let text = "package:com.wolt.courierapp versionCode:240901 installer=com.android.vending\n\
                    package:tech.getsecretary.secretary versionCode:72  installer=null\n\
                    \n\
                    garbage line\n";
        let list = parse_pm_list(text);
        assert_eq!(
            list,
            vec![
                PmPackage {
                    package: "com.wolt.courierapp".into(),
                    version_code: Some(240901),
                    installer: Some("com.android.vending".into())
                },
                PmPackage { package: "tech.getsecretary.secretary".into(), version_code: Some(72), installer: None },
            ]
        );
    }

    #[test]
    fn pm_list_plain_and_exact_match() {
        let list = parse_pm_list("package:com.lucina.maps\npackage:com.lucina.maps.debug\n");
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].version_code, None);
        assert_eq!(find_exact(&list, "com.lucina.maps.debug").unwrap().package, "com.lucina.maps.debug");
        assert!(find_exact(&list, "com.lucina").is_none());
    }

    #[test]
    fn package_names() {
        assert!(is_valid_package_name("com.wolt.courierapp"));
        assert!(is_valid_package_name("android"));
        assert!(is_valid_package_name("com.foo_bar.Baz2"));
        assert!(!is_valid_package_name(""));
        assert!(!is_valid_package_name("com..foo"));
        assert!(!is_valid_package_name("com.1foo"));
        assert!(!is_valid_package_name("com.foo; reboot"));
        assert!(!is_valid_package_name("$(id)"));
    }

    #[test]
    fn version_name_loop() {
        let script = version_name_script(&["com.a.b", "org.c"]).unwrap();
        assert!(script.starts_with("for p in com.a.b org.c; do"));
        assert!(version_name_script(&["com.a; rm -rf /"]).is_err());

        let out = "@@com.a.b\n    versionName=1.2.3 beta\n@@org.c\n@@com.wolt.courierapp\n    versionName=24.9.1\n";
        let names = parse_version_names(out);
        assert_eq!(names.get("com.a.b").map(String::as_str), Some("1.2.3 beta"));
        assert_eq!(names.get("org.c"), None);
        assert_eq!(names.get("com.wolt.courierapp").map(String::as_str), Some("24.9.1"));
    }

    #[test]
    fn badging_with_extra_fields() {
        let text = "package: name='tech.getsecretary.pkgtest' versionCode='2' versionName='2.0' \
                    platformBuildVersionName='13' platformBuildVersionCode='33' compileSdkVersion='33' \
                    compileSdkVersionCodename='13'\nsdkVersion:'29'\ntargetSdkVersion:'33'\n\
                    application-label:'pkgtest'\n";
        assert_eq!(
            parse_badging(text).unwrap(),
            Badging { package: "tech.getsecretary.pkgtest".into(), version_code: 2, version_name: "2.0".into() }
        );
    }

    #[test]
    fn badging_spaces_in_name_and_errors() {
        let b = parse_badging("package: name='a.b' versionCode='10' versionName='1.0 RC 1'\n").unwrap();
        assert_eq!(b.version_name, "1.0 RC 1");
        assert_eq!(parse_badging("package: name='a.b' versionCode='7'\n").unwrap().version_name, "");
        assert!(parse_badging("ERROR: dump failed\n").is_err());
        assert!(parse_badging("package: name='a.b' versionName='1'\n").is_err());
        assert!(parse_badging("package: name='a.b' versionCode='x'\n").is_err());
    }

    #[test]
    fn mdns_lines() {
        let text = "List of discovered mdns services\n\
                    adb-RF8T31C33FW-Ab12Cd\t_adb-tls-connect._tcp\t192.168.178.44:37421\n\
                    adb-RF8T31C33FW-Ab12Cd   _adb-tls-pairing._tcp   192.168.178.44:41011\n\
                    adb-RF8T31C33FW-Ab12Cd    _adb-tls-connect._tcp.    100.89.7.91:40123\n\
                    adb-RF8T31C33FW-Ab12Cd\t_adb-tls-connect._tcp\t192.168.178.44:37421\n";
        assert_eq!(parse_mdns_tls(text), vec!["192.168.178.44:37421", "100.89.7.91:40123"]);
        assert!(parse_mdns_tls("List of discovered mdns services\n").is_empty());
    }

    #[test]
    fn connect_words() {
        assert!(connect_succeeded("connected to 100.89.7.91:5555\n"));
        assert!(connect_succeeded("already connected to 100.89.7.91:5555\n"));
        assert!(!connect_succeeded("failed to connect to '100.89.7.91:5555': Connection refused\n"));
        assert!(!connect_succeeded("cannot connect to 100.89.7.91:5555: No route to host\n"));
        assert!(!connect_succeeded(""));
    }

    #[test]
    fn target_parts() {
        assert_eq!(target_host("100.89.7.91:5555"), "100.89.7.91");
        assert_eq!(target_port("100.89.7.91:5555").unwrap(), 5555);
        assert!(target_port("100.89.7.91").is_err());
    }

    #[test]
    fn gate_idle_sample() {
        let g = wolt_gate(ACTIVITIES_IDLE, SERVICES_IDLE, LOCATION_IDLE);
        assert_eq!(g, Gate { busy: false, reasons: vec![] });
    }

    #[test]
    fn gate_busy_listener_line() {
        let loc = LOCATION_IDLE.replace(
            "    gps provider:\n",
            "    gps provider:\n      listeners:\n        10434/com.wolt.courierapp/1A2B3C Request[@+5s0ms HIGH_ACCURACY, WorkSource{10434 com.wolt.courierapp}]\n",
        );
        let g = wolt_gate(ACTIVITIES_IDLE, SERVICES_IDLE, &loc);
        assert!(g.busy);
        assert_eq!(g.reasons.len(), 1);
        assert!(g.reasons[0].contains("10434/com.wolt.courierapp/1A2B3C"));
    }

    #[test]
    fn gate_busy_provider_request_line() {
        let loc = LOCATION_IDLE.replace(
            "service: ProviderRequest[OFF]",
            "service: ProviderRequest[@+5s0ms, HIGH_ACCURACY, WorkSource{10434 com.wolt.courierapp}]",
        );
        let g = wolt_gate(ACTIVITIES_IDLE, SERVICES_IDLE, &loc);
        assert!(g.busy);
        assert!(g.reasons[0].starts_with("Wolt is requesting location: service: ProviderRequest[@+5s0ms"));
    }

    #[test]
    fn gate_busy_resumed_activity() {
        let acts = ACTIVITIES_IDLE.replace(
            "  topResumedActivity=ActivityRecord{77aa u0 com.sec.android.app.launcher/.activities.LauncherActivity t12}",
            "  topResumedActivity=ActivityRecord{88bb u0 com.wolt.courierapp/.MainActivity t1234}",
        );
        let g = wolt_gate(&acts, SERVICES_IDLE, LOCATION_IDLE);
        assert_eq!(g.reasons, vec!["Wolt is the resumed (on-screen) activity"]);
    }

    #[test]
    fn gate_busy_foreground_service() {
        let svc = SERVICES_IDLE.replace("isForeground=false foregroundId=0", "isForeground=true foregroundId=42");
        let g = wolt_gate(ACTIVITIES_IDLE, &svc, LOCATION_IDLE);
        assert_eq!(g.reasons, vec!["Wolt has a foreground service"]);
        let none = "ACTIVITY MANAGER SERVICES (dumpsys activity services)\n  (nothing)\n";
        assert!(!wolt_gate(ACTIVITIES_IDLE, none, LOCATION_IDLE).busy);
    }

    #[test]
    fn gate_fails_closed_on_empty_text() {
        let g = wolt_gate("", "  \n", "");
        assert!(g.busy);
        assert_eq!(
            g.reasons,
            vec![
                "could not read dumpsys activity activities",
                "could not read dumpsys activity services",
                "could not read dumpsys location",
            ]
        );
        let no_section = wolt_gate(ACTIVITIES_IDLE, SERVICES_IDLE, "Permission Denial: can't dump\n");
        assert!(no_section.busy);
        assert!(no_section.reasons[0].contains("Location Providers"));
    }

    #[test]
    fn downgrade_decision() {
        assert!(check_version(None, 1).is_ok());
        assert!(check_version(Some(1), 2).is_ok());
        assert!(check_version(Some(2), 2).is_ok());
        let err = check_version(Some(3), 2).unwrap_err().to_string();
        assert!(err.starts_with("downgrade refused"), "{err}");
    }

    #[test]
    fn rfc3339() {
        assert_eq!(format_rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_rfc3339(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(format_rfc3339(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(format_rfc3339(1_793_491_199), "2026-10-31T23:59:59Z");
    }
}
