#![allow(dead_code)]

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use async_trait::async_trait;
use lucina_parcels::client::parse_status;
use lucina_parcels::{Status, Track, TrackError};
use serde_json::Value;

pub fn fixture(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/{name}.json", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

pub fn status(name: &str) -> Status {
    parse_status(&fixture(name)).unwrap()
}

/// Answers per tracking number, in order; the last answer repeats. Counts the calls.
#[derive(Default)]
pub struct Fake {
    answers: Mutex<HashMap<String, VecDeque<Result<Status, TrackError>>>>,
    pub calls: Mutex<Vec<String>>,
}

impl Fake {
    pub fn script(&self, number: &str, answers: Vec<Result<Status, TrackError>>) {
        self.answers
            .lock()
            .unwrap()
            .insert(number.to_owned(), answers.into());
    }

    pub fn count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
}

#[async_trait]
impl Track for Fake {
    async fn track(&self, number: &str, _carrier: Option<&str>) -> Result<Status, TrackError> {
        self.calls.lock().unwrap().push(number.to_owned());
        let mut all = self.answers.lock().unwrap();
        let q = all.get_mut(number).expect("a scripted number");
        if q.len() > 1 {
            q.pop_front().unwrap()
        } else {
            q.front().cloned().unwrap()
        }
    }
}
