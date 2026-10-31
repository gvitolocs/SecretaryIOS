// Differential test of `group_exec` (verification/verus/deals_kernel.rs) against the
// private `engine.rs::group_deals`. run-deals.sh appends
// `#[cfg(test)] mod verus_group { include!(".../tests/verus/group.rs"); }` to the
// extracted engine.rs (no existing line changes) and runs `cargo test --lib verus_group`.

#[allow(dead_code, clippy::all)]
mod kernel {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/verus/deals_kernel_exec.rs"
    ));
}

use std::collections::HashMap;

use serde_json::json;

use super::{by_price, group_deals};
use crate::js::{display, truthy};
use crate::sources::{Deal, DealSource, DealStore};
use crate::validity::Status;

fn deal(
    key: String,
    source: DealSource,
    ean: Option<serde_json::Value>,
    name: &str,
    price: f64,
    distance: f64,
) -> Deal {
    Deal {
        key,
        source,
        name: Some(json!(name)),
        chain: None,
        chain_key: "netto".into(),
        dealer_id: None,
        store: DealStore {
            id: None,
            name: None,
            street: None,
            lat: None,
            lng: None,
            distance_m: distance,
        },
        price_dkk: price,
        normal_price_dkk: None,
        discount_pct: None,
        stock: None,
        stock_unit: None,
        valid_from: None,
        valid_to: None,
        ean,
        category: None,
        unit_price: None,
        status: Status::Active,
        score: 1000,
        related_keys: None,
        more_stores: None,
    }
}

struct Ids(HashMap<String, u64>);

impl Ids {
    fn id(&mut self, s: &str) -> u64 {
        let n = self.0.len() as u64;
        *self.0.entry(s.to_owned()).or_insert(n)
    }
}

#[test]
fn group_deals_refines_group_exec() {
    let mut seed: u64 = 0x5DEE_CE66_D;
    let mut next = move |n: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % n
    };
    let mut grouped_members = 0;
    for _ in 0..4_000 {
        let mut deals = Vec::new();
        for _ in 0..next(8) {
            let store = next(4);
            if next(3) == 0 {
                let id = next(5);
                deals.push(deal(
                    format!("tjek:{id}"),
                    DealSource::Flyer,
                    None,
                    "skyr",
                    (1 + next(4)) as f64,
                    next(3) as f64,
                ));
            } else {
                let ean = match next(4) {
                    0 => None,
                    1 => Some(json!("")),
                    _ => Some(json!(format!("57{}", next(3)))),
                };
                let name = ["mælk", "skyr"][next(2) as usize];
                let key = format!(
                    "fw:{store}:{}",
                    if truthy(ean.as_ref()) {
                        display(ean.as_ref())
                    } else {
                        name.to_owned()
                    }
                );
                deals.push(deal(
                    key,
                    DealSource::FoodWaste,
                    ean,
                    name,
                    (1 + next(4)) as f64,
                    next(3) as f64,
                ));
            }
        }
        // The kernel sees the deals in group_deals' order (its first statement is the
        // sort) as (group id, key); the group id is group_deals' own map key.
        let mut sorted = deals.clone();
        sorted.sort_by(by_price);
        let mut ids = Ids(HashMap::new());
        let input: Vec<(u64, u64)> = sorted
            .iter()
            .map(|d| {
                let gid = if d.source == DealSource::FoodWaste {
                    let ean = d.ean.as_ref();
                    format!(
                        "fw:{}",
                        if truthy(ean) {
                            display(ean)
                        } else {
                            display(d.name.as_ref())
                        }
                    )
                } else {
                    d.key.clone()
                };
                (ids.id(&format!("group {gid}")), ids.id(&d.key))
            })
            .collect();
        let (groups, idx) = kernel::group_exec(&input);
        assert_eq!(idx.len(), input.len());
        let real = group_deals(deals);
        assert_eq!(real.len(), groups.len());
        for (_, keys) in &groups {
            let rep = real
                .iter()
                .find(|d| ids.id(&d.key) == keys[0])
                .expect("representative");
            let related: Vec<u64> = rep
                .related_keys
                .as_ref()
                .expect("grouped")
                .iter()
                .map(|k| ids.id(k))
                .collect();
            assert_eq!(&related, keys);
            assert_eq!(rep.more_stores, Some(keys.len() as u32 - 1));
            grouped_members += keys.len() - 1;
        }
        // Every input deal's key is among its group's keys (the Verus postcondition).
        for (i, (_, key)) in input.iter().enumerate() {
            assert!(groups[idx[i]].1.contains(key));
        }
    }
    assert!(
        grouped_members > 1_000,
        "groups with several stores: {grouped_members}"
    );
}
