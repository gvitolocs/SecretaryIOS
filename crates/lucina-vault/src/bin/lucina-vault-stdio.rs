//! `lucina-vault-stdio <vault folder>`: the vault's protocol over stdin and stdout, one JSON request per line in, one answer
//! per line out, for a host that cannot link Rust (the Mac app is Electron). Same core, same answers as `secretary-ffi` and JNI.
use lucina_vault::protocol::Session;
use std::io::{BufRead, Write};

fn main() {
    let Some(root) = std::env::args().nth(1) else {
        eprintln!("usage: lucina-vault-stdio <vault folder>");
        std::process::exit(2);
    };
    let mut session = match Session::open(&root) {
        Ok(s) => s,
        Err(e) => {
            println!(
                "{}",
                serde_json::json!({"ok": false, "code": "not_open", "error": e.to_string()})
            );
            std::process::exit(1);
        }
    };
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if writeln!(out, "{}", session.request(&line))
            .and_then(|_| out.flush())
            .is_err()
        {
            break;
        }
    }
}
