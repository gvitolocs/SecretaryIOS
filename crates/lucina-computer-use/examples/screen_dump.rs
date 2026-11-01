//! Print what the Jev engine sees on a sandbox: `screen_dump desktop|android`.
use lucina_computer_use::jev_drive::{debug_state, read_screen, Where};
#[tokio::main]
async fn main() {
    let t = match std::env::args().nth(1).as_deref() { Some("android") => Where::Android, _ => Where::Desktop };
    let s = read_screen(t).await.expect("screen");
    println!("{}", serde_json::to_string_pretty(&debug_state("(dump)", t, &s)).unwrap());
}
