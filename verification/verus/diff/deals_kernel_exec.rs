// Plain-Rust copy of the exec functions of `verification/verus/deals_kernel.rs`: the
// same bodies with the Verus contracts, invariants and ghost/proof code removed. The
// differential tests (`deals.rs`, `deals_group.rs`) run them against the real
// lucina-deals code. `run-deals.sh` refuses to run when deals_kernel.rs changed since
// this copy was made (`deals-kernel.sha256`): update the copy, then the hash.

pub fn validity_exec(from: i64, to: i64, now: i64) -> u8 {
    if to <= now || to <= from {
        2
    } else if from <= now {
        0
    } else if (from as i128) <= (now as i128) + 259_200_000 {
        1
    } else {
        2
    }
}

pub fn validity_opt_exec(from: Option<i64>, to: Option<i64>, now: i64) -> u8 {
    match (from, to) {
        (Some(f), Some(t)) => validity_exec(f, t, now),
        _ => 2,
    }
}

pub fn append_exec(out: &mut Vec<u8>, tail: &Vec<u8>) {
    let mut i = 0;
    while i < tail.len() {
        out.push(tail[i]);
        i += 1;
    }
}

pub fn wire_exec(food_waste: bool, id: &Vec<u8>, ean: &Vec<u8>) -> Vec<u8> {
    let mut out = Vec::new();
    if food_waste {
        out.push(102);
        out.push(119);
        out.push(58);
        append_exec(&mut out, id);
        out.push(58);
        append_exec(&mut out, ean);
    } else {
        out.push(116);
        out.push(106);
        out.push(101);
        out.push(107);
        out.push(58);
        append_exec(&mut out, id);
    }
    out
}

pub fn contains_exec(v: &Vec<u64>, k: u64) -> bool {
    let mut i = 0;
    while i < v.len() {
        if v[i] == k {
            return true;
        }
        i += 1;
    }
    false
}

pub fn deal_keys_exec(key: u64, related: &Vec<u64>) -> Vec<u64> {
    let mut out: Vec<u64> = Vec::new();
    out.push(key);
    let mut i = 0;
    while i < related.len() {
        let k = related[i];
        if !contains_exec(&out, k) {
            out.push(k);
        }
        i += 1;
    }
    out
}

fn append_u64(out: &mut Vec<u64>, tail: &Vec<u64>) {
    let mut i = 0;
    while i < tail.len() {
        out.push(tail[i]);
        i += 1;
    }
}

pub fn copy_exec(v: &Vec<u64>) -> Vec<u64> {
    let mut out = Vec::new();
    append_u64(&mut out, v);
    out
}

fn find_group(out: &Vec<(u64, Vec<u64>)>, gid: u64) -> Option<usize> {
    let mut j = 0;
    while j < out.len() {
        if out[j].0 == gid {
            return Some(j);
        }
        j += 1;
    }
    None
}

pub fn group_exec(deals: &Vec<(u64, u64)>) -> (Vec<(u64, Vec<u64>)>, Vec<usize>) {
    let mut out: Vec<(u64, Vec<u64>)> = Vec::new();
    let mut idx: Vec<usize> = Vec::new();
    let mut i = 0;
    while i < deals.len() {
        let (gid, key) = deals[i];
        match find_group(&out, gid) {
            None => {
                let mut keys = Vec::new();
                keys.push(key);
                out.push((gid, keys));
                idx.push(out.len() - 1);
            }
            Some(j) => {
                if key != out[j].1[0] {
                    let mut keys = copy_exec(&out[j].1);
                    keys.push(key);
                    out[j] = (gid, keys);
                }
                idx.push(j);
            }
        }
        i += 1;
    }
    (out, idx)
}

pub fn new_exec(old: &Vec<Option<i64>>, price: i64) -> bool {
    let mut i = 0;
    while i < old.len() {
        if let Some(p) = old[i] {
            if p <= price {
                return false;
            }
        }
        i += 1;
    }
    true
}

pub fn select_exec(olds: &Vec<Vec<Option<i64>>>, prices: &Vec<i64>) -> Vec<bool> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < prices.len() {
        out.push(new_exec(&olds[i], prices[i]));
        i += 1;
    }
    out
}

pub fn eq_exec(a: &Vec<char>, b: &Vec<char>) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

pub fn ends_exec(w: &Vec<char>, t: &Vec<char>) -> bool {
    if t.len() > w.len() {
        return false;
    }
    let off = w.len() - t.len();
    let mut i = 0;
    while i < t.len() {
        if w[off + i] != t[i] {
            return false;
        }
        i += 1;
    }
    true
}

pub fn head_final_exec(w: &Vec<char>, flags: &Vec<bool>, t: &Vec<char>) -> bool {
    if !ends_exec(w, t) {
        return false;
    }
    let n = w.len() - t.len();
    if n < 2 {
        return false;
    }
    let mut i = 0;
    while i < n {
        if !flags[i] {
            return false;
        }
        i += 1;
    }
    true
}

fn phrase_at_exec(words: &Vec<Vec<char>>, needle: &Vec<Vec<char>>, i: usize) -> bool {
    let mut j = 0;
    while j < needle.len() {
        let same = eq_exec(&words[i + j], &needle[j]);
        if !same {
            return false;
        }
        j += 1;
    }
    true
}

fn min400_exec(i: usize) -> i32 {
    if i < 400 {
        i as i32
    } else {
        400
    }
}

pub fn term_score_exec(
    words: &Vec<Vec<char>>,
    flags: &Vec<Vec<bool>>,
    needle: &Vec<Vec<char>>,
) -> i32 {
    if needle.len() == 0 {
        return 0;
    }
    if words.len() >= needle.len() {
        let mut i = 0;
        while i <= words.len() - needle.len() {
            if phrase_at_exec(words, needle, i) {
                return 1000 - min400_exec(i);
            }
            i += 1;
        }
    }
    if needle.len() == 1 {
        let mut i = 0;
        while i < words.len() {
            if head_final_exec(&words[i], &flags[i], &needle[0]) {
                return 500 - min400_exec(i);
            }
            i += 1;
        }
    }
    0
}

pub fn match_deal_exec(
    words: &Vec<Vec<char>>,
    flags: &Vec<Vec<bool>>,
    excludes: &Vec<Vec<Vec<char>>>,
    category_ok: bool,
    terms: &Vec<Vec<Vec<char>>>,
) -> i32 {
    let mut e = 0;
    while e < excludes.len() {
        let s = term_score_exec(words, flags, &excludes[e]);
        if s != 0 {
            return 0;
        }
        e += 1;
    }
    if !category_ok {
        return 0;
    }
    let mut best: i32 = 0;
    let mut t = 0;
    while t < terms.len() {
        let s = term_score_exec(words, flags, &terms[t]);
        if s > best {
            best = s;
        }
        t += 1;
    }
    best
}
