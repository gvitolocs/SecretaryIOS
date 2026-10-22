//! A subset of Todoist's filter queries, evaluated locally so saved filters
//! open offline: `today`, `tomorrow`, `overdue`/`od`, `no date`,
//! `recurring`, `subtask`, `no labels`, `p1`…`p4`, `no priority`,
//! `7 days` / `next 3 days`, `@label` (`@lang*`), `#Project`, `##Project`
//! (with sub-projects), `/Section`, `search: text`, `due before: <date>`,
//! `due after: <date>`, `all`, combined with `&`, `|`, `!`, parentheses and
//! `,` (several lists, shown as one). A few Italian and Danish words too.
//! Unknown terms match nothing and are reported in the view's `warning`.

use chrono::{Duration, NaiveDate};
use chrono_tz::Tz;

use crate::models::{State, Task};
use crate::nlp;

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    Or(Vec<Expr>),
    And(Vec<Expr>),
    Not(Box<Expr>),
    Term(String),
}

pub fn parse(q: &str) -> Expr {
    let toks = lex(q);
    let mut i = 0;
    let e = or(&toks, &mut i);
    e.unwrap_or(Expr::Or(vec![]))
}

#[derive(Debug, Clone, PartialEq)]
enum T {
    Or,
    And,
    Not,
    Open,
    Close,
    Term(String),
}

fn lex(q: &str) -> Vec<T> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let flush = |cur: &mut String, out: &mut Vec<T>| {
        let t = cur.trim();
        if !t.is_empty() {
            out.push(T::Term(t.to_owned()));
        }
        cur.clear();
    };
    for c in q.chars() {
        match c {
            '|' | ',' => {
                flush(&mut cur, &mut out);
                out.push(T::Or);
            }
            '&' => {
                flush(&mut cur, &mut out);
                out.push(T::And);
            }
            '!' if cur.trim().is_empty() => {
                flush(&mut cur, &mut out);
                out.push(T::Not);
            }
            '(' => {
                flush(&mut cur, &mut out);
                out.push(T::Open);
            }
            ')' => {
                flush(&mut cur, &mut out);
                out.push(T::Close);
            }
            _ => cur.push(c),
        }
    }
    flush(&mut cur, &mut out);
    out
}

fn or(t: &[T], i: &mut usize) -> Option<Expr> {
    let mut parts = vec![and(t, i)?];
    while t.get(*i) == Some(&T::Or) {
        *i += 1;
        parts.push(and(t, i)?);
    }
    Some(if parts.len() == 1 {
        parts.remove(0)
    } else {
        Expr::Or(parts)
    })
}

fn and(t: &[T], i: &mut usize) -> Option<Expr> {
    let mut parts = vec![unary(t, i)?];
    while t.get(*i) == Some(&T::And) {
        *i += 1;
        parts.push(unary(t, i)?);
    }
    Some(if parts.len() == 1 {
        parts.remove(0)
    } else {
        Expr::And(parts)
    })
}

fn unary(t: &[T], i: &mut usize) -> Option<Expr> {
    match t.get(*i)? {
        T::Not => {
            *i += 1;
            Some(Expr::Not(Box::new(unary(t, i)?)))
        }
        T::Open => {
            *i += 1;
            let e = or(t, i)?;
            if t.get(*i) == Some(&T::Close) {
                *i += 1;
            }
            Some(e)
        }
        T::Term(s) => {
            *i += 1;
            Some(Expr::Term(s.clone()))
        }
        _ => None,
    }
}

pub struct Eval<'a> {
    pub state: &'a State,
    pub tz: Tz,
    pub today: NaiveDate,
    pub unsupported: std::cell::RefCell<Vec<String>>,
}

impl Eval<'_> {
    fn due(&self, t: &Task) -> Option<NaiveDate> {
        t.due
            .as_ref()
            .and_then(|d| crate::due_local(d, self.tz))
            .map(|(d, _)| d)
    }

    fn project_and_children(&self, name: &str) -> Vec<String> {
        let mut out: Vec<String> = self
            .state
            .projects
            .values()
            .filter(|p| p.name.eq_ignore_ascii_case(name))
            .map(|p| p.id.clone())
            .collect();
        let mut i = 0;
        while i < out.len() {
            let id = out[i].clone();
            out.extend(
                self.state
                    .projects
                    .values()
                    .filter(|p| p.parent_id.as_deref() == Some(id.as_str()))
                    .map(|p| p.id.clone()),
            );
            i += 1;
        }
        out
    }

    pub fn matches(&self, e: &Expr, t: &Task) -> bool {
        match e {
            Expr::Or(v) => v.iter().any(|x| self.matches(x, t)),
            Expr::And(v) => v.iter().all(|x| self.matches(x, t)),
            Expr::Not(x) => !self.matches(x, t),
            Expr::Term(s) => self.term(s, t),
        }
    }

    fn term(&self, raw: &str, t: &Task) -> bool {
        let s = raw.trim().to_lowercase();
        let due = self.due(t);
        let today = self.today;
        match s.as_str() {
            "all" | "tutti" | "alle" => return true,
            "today" | "oggi" | "i dag" => return due == Some(today),
            "tomorrow" | "domani" | "i morgen" => return due == Some(today + Duration::days(1)),
            "overdue" | "od" | "scaduto" | "scaduti" | "in ritardo" | "forfalden" | "forfaldne" => {
                return due.is_some_and(|d| d < today)
            }
            "no date" | "no due date" | "senza data" | "ingen dato" => return t.due.is_none(),
            "recurring" | "ricorrente" | "ricorrenti" | "gentagende" => return t.recurring(),
            "subtask" | "subtasks" => return t.parent_id.is_some(),
            "no labels" | "senza etichette" | "ingen etiketter" => return t.labels.is_empty(),
            "no priority" => return t.priority == 1,
            "p1" => return t.priority == 4,
            "p2" => return t.priority == 3,
            "p3" => return t.priority == 2,
            "p4" => return t.priority == 1,
            _ => {}
        }
        // N days / next N days
        let words: Vec<&str> = s.split_whitespace().collect();
        let n_days = match words.as_slice() {
            [n, "days" | "giorni" | "dage"]
            | ["next", n, "days"]
            | ["prossimi", n, "giorni"]
            | ["næste", n, "dage"] => n.parse::<i64>().ok(),
            _ => None,
        };
        if let Some(n) = n_days {
            return due.is_some_and(|d| d >= today && d < today + Duration::days(n.max(1)));
        }
        if let Some(name) = s.strip_prefix("##") {
            return self
                .project_and_children(name.trim())
                .contains(&t.project_id);
        }
        if let Some(name) = s.strip_prefix('#') {
            let name = name.trim();
            return self
                .state
                .projects
                .get(&t.project_id)
                .is_some_and(|p| p.name.eq_ignore_ascii_case(name))
                || (name == "inbox" && self.state.inbox_id() == Some(t.project_id.as_str()));
        }
        if let Some(name) = s.strip_prefix('@') {
            let name = name.trim();
            return t.labels.iter().any(|l| {
                let l = l.to_lowercase();
                match name.strip_suffix('*') {
                    Some(prefix) => l.starts_with(prefix),
                    None => l == name,
                }
            });
        }
        if let Some(name) = s.strip_prefix('/') {
            let name = name.trim();
            return t
                .section_id
                .as_ref()
                .and_then(|id| self.state.sections.get(id))
                .is_some_and(|x| x.name.eq_ignore_ascii_case(name));
        }
        if let Some(q) = s
            .strip_prefix("search:")
            .or_else(|| s.strip_prefix("cerca:"))
            .or_else(|| s.strip_prefix("søg:"))
        {
            let q = q.trim();
            return t.content.to_lowercase().contains(q)
                || t.description.to_lowercase().contains(q);
        }
        for (prefix, before) in [("due before:", true), ("due after:", false)] {
            if let Some(rest) = s.strip_prefix(prefix) {
                let ctx = nlp::Ctx::new(today);
                let bound = NaiveDate::parse_from_str(rest.trim(), "%Y-%m-%d")
                    .ok()
                    .or_else(|| nlp::parse_due(rest.trim(), ctx).map(|m| m.date));
                return match (bound, due) {
                    (Some(b), Some(d)) => {
                        if before {
                            d < b
                        } else {
                            d > b
                        }
                    }
                    _ => false,
                };
            }
        }
        let mut u = self.unsupported.borrow_mut();
        if !u.contains(&raw.trim().to_owned()) {
            u.push(raw.trim().to_owned());
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_precedence() {
        assert_eq!(
            parse("today | overdue & p1"),
            Expr::Or(vec![
                Expr::Term("today".into()),
                Expr::And(vec![Expr::Term("overdue".into()), Expr::Term("p1".into())])
            ])
        );
        assert_eq!(
            parse("!(#Work | @home)"),
            Expr::Not(Box::new(Expr::Or(vec![
                Expr::Term("#Work".into()),
                Expr::Term("@home".into())
            ])))
        );
        assert_eq!(parse("today, overdue"), parse("today | overdue"));
    }
}
