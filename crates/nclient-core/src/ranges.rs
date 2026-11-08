//! Port of `api/components/Ranges.java`.
//!
//! Java models dates as `int + TimeUnit` with `null` unit meaning "unset";
//! setting a date without a unit NPEs in `toQuery`. The port models an unset
//! date as `None` and a set date as `Some((value, unit))`, so the NPE state is
//! unrepresentable and `to_query` is total. All other behavior is verbatim.

use serde::{Deserialize, Serialize};

pub const UNDEFINED: i32 = -1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimeUnit {
    Hour, // 'h'
    Day,  // 'd'
    Week, // 'w'
    Month, // 'm'
    Year, // 'y'
}

impl TimeUnit {
    /// `getVal()`
    pub fn val(self) -> char {
        match self {
            TimeUnit::Hour => 'h',
            TimeUnit::Day => 'd',
            TimeUnit::Week => 'w',
            TimeUnit::Month => 'm',
            TimeUnit::Year => 'y',
        }
    }
}

/// `Ranges` — pages are `i32` with `UNDEFINED = -1` as the unset marker,
/// exactly like Java.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Ranges {
    pub from_page: i32,
    pub to_page: i32,
    /// Java: `fromDate` int + `fromDateUnit` (nullable). `None` = both unset.
    pub from_date: Option<(i32, TimeUnit)>,
    pub to_date: Option<(i32, TimeUnit)>,
}

impl Ranges {
    pub fn new() -> Ranges {
        Ranges {
            from_page: UNDEFINED,
            to_page: UNDEFINED,
            from_date: None,
            to_date: None,
        }
    }

    /// `isDefault`
    pub fn is_default(&self) -> bool {
        self.from_date.is_none()
            && self.to_date.is_none()
            && self.to_page == UNDEFINED
            && self.from_page == UNDEFINED
    }

    /// `toQuery` — byte-for-byte port including the space policy (spaces as
    /// separators, then a final `trim()`). Callers append this after a `+` in
    /// the search URL, and `InspectorV3.createUrl` later replaces remaining
    /// spaces with `+`.
    pub fn to_query(&self) -> String {
        let mut s = String::new();
        if self.from_page != UNDEFINED && self.to_page != UNDEFINED && self.from_page == self.to_page
        {
            s.push_str(&format!("pages:{} ", self.from_page));
        } else {
            if self.from_page != UNDEFINED {
                s.push_str(&format!("pages:>={} ", self.from_page));
            }
            if self.to_page != UNDEFINED {
                s.push_str(&format!("pages:<={} ", self.to_page));
            }
        }

        match (self.from_date, self.to_date) {
            (Some((f, fu)), Some((t, _tu))) if f == t => {
                // Java uses fromDateUnit here; f == t implies same query value
                // only when units agree — the UI guarantees that; mirror the
                // Java read of fromDateUnit.
                s.push_str(&format!("uploaded:{}{}", f, fu.val()));
            }
            (f, t) => {
                if let Some((v, u)) = f {
                    s.push_str(&format!("uploaded:>={}{} ", v, u.val()));
                }
                if let Some((v, u)) = t {
                    s.push_str(&format!("uploaded:<={}{}", v, u.val()));
                }
            }
        }
        s.trim().to_string()
    }
}
