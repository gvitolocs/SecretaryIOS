//! Times `merge` on a realistic load: 255 current keys (five of them local bubbles) and 200 incoming keys, half of
//! them already in `current`. `cargo run --release -p secretary-chat --example merge_speed`.
use secretary_chat::merge::{merge, Key, Role};

fn key(id: String, secs: i64, role: Role, client: Option<String>) -> Key {
    Key {
        id,
        ts_us: secs * 1_000_000,
        role,
        client_message_id: client,
        reply_to: None,
        share_id: None,
    }
}

fn main() {
    let mut current: Vec<Key> = (0..250)
        .map(|i| key(format!("tl_{i:04}"), i, Role::User, None))
        .collect();
    for i in 0..5 {
        current.push(key(
            format!("local-{i}"),
            300 + i,
            Role::User,
            Some(format!("c{i}")),
        ));
    }
    let incoming: Vec<Key> = (150..350)
        .map(|i| {
            let role = if i % 3 == 0 { Role::Lucina } else { Role::User };
            let client = (i >= 345).then(|| format!("c{}", i - 345));
            key(format!("tl_{i:04}"), i, role, client)
        })
        .collect();
    let runs: u32 = 2000;
    let t = std::time::Instant::now();
    let mut n = 0;
    for _ in 0..runs {
        n += merge(
            std::hint::black_box(&current),
            std::hint::black_box(&incoming),
        )
        .ids
        .len();
    }
    println!(
        "merge of {} current and {} incoming keys: {:?} per call ({} ids)",
        current.len(),
        incoming.len(),
        t.elapsed() / runs,
        n / runs as usize
    );
}
