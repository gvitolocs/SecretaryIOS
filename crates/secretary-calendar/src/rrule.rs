//! An RRULE subset (RFC 5545 §3.3.10), expanded in wall-clock time.
//!
//! Supported: `FREQ=DAILY|WEEKLY|MONTHLY|YEARLY`, `INTERVAL`, `COUNT`,
//! `UNTIL` (date, UTC or floating), `BYDAY` (with ordinals such as `2TU`,
//! `-1FR` for monthly/yearly), `BYMONTHDAY` (negative counts from the end),
//! `BYMONTH`, `BYSETPOS`, `WKST`. Not supported (the rule is refused, and the
//! caller shows only the first instance): `SECONDLY`/`MINUTELY`/`HOURLY`,
//! `BYHOUR`, `BYMINUTE`, `BYSECOND`, `BYWEEKNO`, `BYYEARDAY`.
//!
//! DTSTART is always the first instance (RFC 5545), even when it does not
//! match the rule.

use chrono::{Datelike, Duration, Months, NaiveDate, NaiveDateTime, Weekday};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freq {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

/// The end of a rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Until {
    Date(NaiveDate),
    /// `UNTIL=…Z`: compared through the caller's wall → UTC conversion.
    Utc(NaiveDateTime),
    /// Floating `UNTIL`: compared as wall-clock time.
    Local(NaiveDateTime),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RRule {
    pub freq: Freq,
    pub interval: u32,
    pub count: Option<u32>,
    pub until: Option<Until>,
    pub by_day: Vec<(Option<i32>, Weekday)>,
    pub by_month_day: Vec<i32>,
    pub by_month: Vec<u32>,
    pub by_set_pos: Vec<i32>,
    pub wkst: Weekday,
}

fn weekday(s: &str) -> Option<Weekday> {
    Some(match s {
        "MO" => Weekday::Mon,
        "TU" => Weekday::Tue,
        "WE" => Weekday::Wed,
        "TH" => Weekday::Thu,
        "FR" => Weekday::Fri,
        "SA" => Weekday::Sat,
        "SU" => Weekday::Sun,
        _ => return None,
    })
}

fn ints(v: &str, lo: i32, hi: i32, allow_neg: bool) -> Result<Vec<i32>, String> {
    v.split(',')
        .map(|p| {
            let n: i32 = p.trim().parse().map_err(|_| format!("bad number {p:?}"))?;
            let a = n.abs();
            if a < lo || a > hi || (n < 0 && !allow_neg) {
                return Err(format!("{n} out of range"));
            }
            Ok(n)
        })
        .collect()
}

/// Parse `[RRULE:]FREQ=…;…`.
pub fn parse(s: &str) -> Result<RRule, String> {
    let body = s.trim();
    let body = body
        .strip_prefix("RRULE:")
        .or_else(|| body.strip_prefix("rrule:"))
        .unwrap_or(body);
    let mut freq = None;
    let mut r = RRule {
        freq: Freq::Daily,
        interval: 1,
        count: None,
        until: None,
        by_day: Vec::new(),
        by_month_day: Vec::new(),
        by_month: Vec::new(),
        by_set_pos: Vec::new(),
        wkst: Weekday::Mon,
    };
    for part in body.split(';').filter(|p| !p.trim().is_empty()) {
        let (k, v) = part
            .split_once('=')
            .ok_or_else(|| format!("bad rule part {part:?}"))?;
        let v = v.trim();
        match k.trim().to_ascii_uppercase().as_str() {
            "FREQ" => {
                freq = Some(match v.to_ascii_uppercase().as_str() {
                    "DAILY" => Freq::Daily,
                    "WEEKLY" => Freq::Weekly,
                    "MONTHLY" => Freq::Monthly,
                    "YEARLY" => Freq::Yearly,
                    other => return Err(format!("FREQ={other} is not supported")),
                })
            }
            "INTERVAL" => {
                r.interval = v
                    .parse()
                    .ok()
                    .filter(|n| (1..=1000).contains(n))
                    .ok_or("bad INTERVAL")?
            }
            "COUNT" => {
                r.count = Some(
                    v.parse()
                        .ok()
                        .filter(|n| (1..=100_000).contains(n))
                        .ok_or("bad COUNT")?,
                )
            }
            "UNTIL" => r.until = Some(parse_until(v)?),
            "BYDAY" => {
                for d in v.split(',') {
                    let d = d.trim().to_ascii_uppercase();
                    if d.len() < 2 {
                        return Err(format!("bad BYDAY {d:?}"));
                    }
                    let (num, wd) = d.split_at(d.len() - 2);
                    let wd = weekday(wd).ok_or_else(|| format!("bad BYDAY {d:?}"))?;
                    let ord = if num.is_empty() {
                        None
                    } else {
                        let n: i32 = num.parse().map_err(|_| format!("bad BYDAY {d:?}"))?;
                        if n == 0 || n.abs() > 53 {
                            return Err(format!("bad BYDAY {d:?}"));
                        }
                        Some(n)
                    };
                    r.by_day.push((ord, wd));
                }
            }
            "BYMONTHDAY" => r.by_month_day = ints(v, 1, 31, true)?,
            "BYMONTH" => {
                r.by_month = ints(v, 1, 12, false)?
                    .into_iter()
                    .map(|n| n as u32)
                    .collect()
            }
            "BYSETPOS" => r.by_set_pos = ints(v, 1, 366, true)?,
            "WKST" => {
                r.wkst = weekday(&v.to_ascii_uppercase()).ok_or("bad WKST")?;
            }
            "BYHOUR" | "BYMINUTE" | "BYSECOND" | "BYWEEKNO" | "BYYEARDAY" => {
                return Err(format!("{k} is not supported"))
            }
            _ => {} // X- names and the like: ignore
        }
    }
    r.freq = freq.ok_or("FREQ is required")?;
    Ok(r)
}

fn parse_until(v: &str) -> Result<Until, String> {
    let v = v.trim();
    if v.len() == 8 {
        return NaiveDate::parse_from_str(v, "%Y%m%d")
            .map(Until::Date)
            .map_err(|_| format!("bad UNTIL {v:?}"));
    }
    if let Some(utc) = v.strip_suffix('Z') {
        return NaiveDateTime::parse_from_str(utc, "%Y%m%dT%H%M%S")
            .map(Until::Utc)
            .map_err(|_| format!("bad UNTIL {v:?}"));
    }
    NaiveDateTime::parse_from_str(v, "%Y%m%dT%H%M%S")
        .map(Until::Local)
        .map_err(|_| format!("bad UNTIL {v:?}"))
}

fn last_day(y: i32, m: u32) -> u32 {
    let next = if m == 12 {
        NaiveDate::from_ymd_opt(y + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(y, m + 1, 1)
    };
    next.and_then(|d| d.pred_opt()).map_or(28, |d| d.day())
}

/// Dates of month `y-m` selected by the BY* parts (or `default_day`).
fn month_dates(r: &RRule, y: i32, m: u32, default_day: u32) -> Vec<NaiveDate> {
    let last = last_day(y, m);
    let date = |d: u32| NaiveDate::from_ymd_opt(y, m, d);
    let mut out: Vec<NaiveDate> = Vec::new();
    if !r.by_month_day.is_empty() {
        for &n in &r.by_month_day {
            let d = if n > 0 {
                n as u32
            } else {
                let back = (-n) as u32;
                if back > last {
                    continue;
                }
                last + 1 - back
            };
            if d <= last {
                if let Some(x) = date(d) {
                    out.push(x);
                }
            }
        }
        if !r.by_day.is_empty() {
            out.retain(|d| r.by_day.iter().any(|(_, wd)| d.weekday() == *wd));
        }
    } else if !r.by_day.is_empty() {
        for &(ord, wd) in &r.by_day {
            let all: Vec<NaiveDate> = (1..=last)
                .filter_map(date)
                .filter(|d| d.weekday() == wd)
                .collect();
            match ord {
                None => out.extend(all),
                Some(n) if n > 0 => out.extend(all.get(n as usize - 1).copied()),
                Some(n) => {
                    let back = (-n) as usize;
                    if back <= all.len() {
                        out.push(all[all.len() - back]);
                    }
                }
            }
        }
    } else if default_day <= last {
        out.extend(date(default_day));
    }
    out
}

fn set_pos(r: &RRule, mut v: Vec<NaiveDate>) -> Vec<NaiveDate> {
    v.sort();
    v.dedup();
    if r.by_set_pos.is_empty() {
        return v;
    }
    let mut out = Vec::new();
    for &p in &r.by_set_pos {
        let idx = if p > 0 {
            (p as usize).checked_sub(1)
        } else {
            v.len().checked_sub((-p) as usize)
        };
        if let Some(d) = idx.and_then(|i| v.get(i)) {
            out.push(*d);
        }
    }
    out.sort();
    out.dedup();
    out
}

/// The candidate dates of period `k` (0 = the period holding DTSTART).
fn period(r: &RRule, start: NaiveDate, k: u64) -> Option<Vec<NaiveDate>> {
    let step = u64::from(r.interval).checked_mul(k)?;
    let month_ok = |d: &NaiveDate| r.by_month.is_empty() || r.by_month.contains(&d.month());
    Some(match r.freq {
        Freq::Daily => {
            let d = start.checked_add_signed(Duration::days(i64::try_from(step).ok()?))?;
            let keep = month_ok(&d)
                && (r.by_month_day.is_empty() || {
                    let last = last_day(d.year(), d.month()) as i32;
                    r.by_month_day.iter().any(|&n| {
                        let want = if n > 0 { n } else { last + 1 + n };
                        want == d.day() as i32
                    })
                })
                && (r.by_day.is_empty() || r.by_day.iter().any(|(_, wd)| *wd == d.weekday()));
            if keep {
                vec![d]
            } else {
                vec![]
            }
        }
        Freq::Weekly => {
            let back =
                (7 + start.weekday().num_days_from_monday() - r.wkst.num_days_from_monday()) % 7;
            let week = start.checked_sub_signed(Duration::days(i64::from(back)))?;
            let week = week.checked_add_signed(Duration::days(i64::try_from(step).ok()? * 7))?;
            let days: Vec<Weekday> = if r.by_day.is_empty() {
                vec![start.weekday()]
            } else {
                r.by_day.iter().map(|(_, wd)| *wd).collect()
            };
            let v = (0..7)
                .filter_map(|i| week.checked_add_signed(Duration::days(i)))
                .filter(|d| days.contains(&d.weekday()) && month_ok(d))
                .collect();
            set_pos(r, v)
        }
        Freq::Monthly => {
            let first = NaiveDate::from_ymd_opt(start.year(), start.month(), 1)?
                .checked_add_months(Months::new(u32::try_from(step).ok()?))?;
            if !month_ok(&first) {
                return Some(vec![]);
            }
            set_pos(r, month_dates(r, first.year(), first.month(), start.day()))
        }
        Freq::Yearly => {
            let y = start.year().checked_add(i32::try_from(step).ok()?)?;
            let months: Vec<u32> = if r.by_month.is_empty() {
                vec![start.month()]
            } else {
                r.by_month.clone()
            };
            let mut v = Vec::new();
            for m in months {
                v.extend(month_dates(r, y, m, start.day()));
            }
            set_pos(r, v)
        }
    })
}

/// Expand `rule` from `dtstart` (wall-clock) until an occurrence passes
/// `end` (wall-clock, exclusive) or the rule ends. `to_utc` converts a wall
/// time to UTC wall time (for `UNTIL=…Z`). At most `limit` occurrences are
/// returned; COUNT still counts the ones before any window the caller
/// applies.
pub fn expand(
    rule: &RRule,
    dtstart: NaiveDateTime,
    end: NaiveDateTime,
    limit: usize,
    to_utc: impl Fn(NaiveDateTime) -> NaiveDateTime,
) -> Vec<NaiveDateTime> {
    let time = dtstart.time();
    let start = dtstart.date();
    let mut out = vec![dtstart];
    let mut count: u32 = 1;
    let past_until = |t: NaiveDateTime| match rule.until {
        None => false,
        Some(Until::Date(d)) => t.date() > d,
        Some(Until::Local(u)) => t > u,
        Some(Until::Utc(u)) => to_utc(t) > u,
    };
    if past_until(dtstart) || dtstart >= end || rule.count == Some(1) {
        return out;
    }
    // Enough periods for a daily rule over ~300 years; empty periods (a
    // BYMONTHDAY=31 in short months) are cheap.
    let mut empty_run = 0u32;
    for k in 0..120_000u64 {
        let Some(dates) = period(rule, start, k) else {
            break;
        };
        if dates.is_empty() {
            empty_run += 1;
            // A rule that matches nothing (BYMONTHDAY=30 with BYMONTH=2).
            if empty_run > 2000 {
                break;
            }
            continue;
        }
        empty_run = 0;
        for d in dates {
            let t = d.and_time(time);
            if t <= dtstart {
                continue;
            }
            if past_until(t) || t >= end {
                return out;
            }
            count += 1;
            if rule.count.is_some_and(|c| count > c) {
                return out;
            }
            out.push(t);
            if out.len() >= limit {
                return out;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()
    }

    fn run(rule: &str, start: &str, end: &str) -> Vec<String> {
        let r = parse(rule).unwrap();
        expand(&r, dt(start), dt(end), 1000, |t| t)
            .into_iter()
            .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
            .collect()
    }

    #[test]
    fn daily_interval_count() {
        assert_eq!(
            run(
                "FREQ=DAILY;INTERVAL=2;COUNT=3",
                "2026-10-20 09:00",
                "2027-01-01 00:00"
            ),
            ["2026-10-20 09:00", "2026-10-22 09:00", "2026-10-24 09:00"]
        );
    }

    #[test]
    fn weekly_byday_until() {
        assert_eq!(
            run(
                "RRULE:FREQ=WEEKLY;BYDAY=MO,WE;UNTIL=20261029",
                "2026-10-19 18:00",
                "2027-01-01 00:00"
            ),
            [
                "2026-10-19 18:00",
                "2026-10-21 18:00",
                "2026-10-26 18:00",
                "2026-10-28 18:00"
            ]
        );
        // every other week, starting on a Wednesday with WKST=MO
        assert_eq!(
            run(
                "FREQ=WEEKLY;INTERVAL=2;BYDAY=WE,FR",
                "2026-10-21 08:00",
                "2026-11-07 00:00"
            ),
            [
                "2026-10-21 08:00",
                "2026-10-23 08:00",
                "2026-11-04 08:00",
                "2026-11-06 08:00"
            ]
        );
    }

    #[test]
    fn monthly_ordinals_and_negative_days() {
        // second Tuesday
        assert_eq!(
            run(
                "FREQ=MONTHLY;BYDAY=2TU;COUNT=3",
                "2026-10-13 10:00",
                "2030-01-01 00:00"
            ),
            ["2026-10-13 10:00", "2026-11-10 10:00", "2026-12-08 10:00"]
        );
        // last Friday
        assert_eq!(
            run(
                "FREQ=MONTHLY;BYDAY=-1FR;COUNT=2",
                "2026-10-30 10:00",
                "2030-01-01 00:00"
            ),
            ["2026-10-30 10:00", "2026-11-27 10:00"]
        );
        // last day of the month
        assert_eq!(
            run(
                "FREQ=MONTHLY;BYMONTHDAY=-1;COUNT=3",
                "2027-01-31 07:00",
                "2030-01-01 00:00"
            ),
            ["2027-01-31 07:00", "2027-02-28 07:00", "2027-03-31 07:00"]
        );
        // the 31st skips short months
        assert_eq!(
            run(
                "FREQ=MONTHLY;COUNT=3",
                "2026-10-31 07:00",
                "2030-01-01 00:00"
            ),
            ["2026-10-31 07:00", "2026-12-31 07:00", "2027-01-31 07:00"]
        );
        // last weekday of the month
        assert_eq!(
            run(
                "FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1;COUNT=2",
                "2026-10-30 17:00",
                "2030-01-01 00:00"
            ),
            ["2026-10-30 17:00", "2026-11-30 17:00"]
        );
    }

    #[test]
    fn yearly_and_leap_day() {
        assert_eq!(
            run(
                "FREQ=YEARLY;COUNT=3",
                "2028-02-29 00:00",
                "2040-01-01 00:00"
            ),
            ["2028-02-29 00:00", "2032-02-29 00:00", "2036-02-29 00:00"]
        );
        // Thanksgiving: fourth Thursday of November
        assert_eq!(
            run(
                "FREQ=YEARLY;BYMONTH=11;BYDAY=4TH;COUNT=2",
                "2026-11-26 12:00",
                "2040-01-01 00:00"
            ),
            ["2026-11-26 12:00", "2027-11-25 12:00"]
        );
    }

    #[test]
    fn window_end_and_refusals() {
        assert_eq!(
            run("FREQ=DAILY", "2026-10-20 09:00", "2026-10-23 09:00"),
            ["2026-10-20 09:00", "2026-10-21 09:00", "2026-10-22 09:00"]
        );
        assert!(parse("FREQ=HOURLY").is_err());
        assert!(parse("FREQ=DAILY;BYHOUR=9").is_err());
        assert!(parse("INTERVAL=2").is_err());
        assert!(parse("FREQ=MONTHLY;BYDAY=0MO").is_err());
        // a rule that never matches stops instead of spinning
        assert_eq!(
            run(
                "FREQ=MONTHLY;BYMONTH=2;BYMONTHDAY=30",
                "2026-01-30 09:00",
                "2400-01-01 00:00"
            ),
            ["2026-01-30 09:00"]
        );
    }

    #[test]
    fn until_in_utc() {
        let r = parse("FREQ=DAILY;UNTIL=20261022T070000Z").unwrap();
        // wall = UTC+2: 09:00 local is 07:00Z, so the 22nd is the last one.
        let got = expand(
            &r,
            dt("2026-10-20 09:00"),
            dt("2027-01-01 00:00"),
            100,
            |t| t - Duration::hours(2),
        );
        assert_eq!(got.len(), 3);
    }
}
