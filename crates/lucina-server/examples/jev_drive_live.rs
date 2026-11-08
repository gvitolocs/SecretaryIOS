//! Live end-to-end: one computer task through `run_task` (Jev engine first, Hermes after).
//! Needs JEV_API_KEY and DEEPSEEK_API_KEY. `jev_drive_live "<task>"`.
use std::sync::Arc;
#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().with_env_filter("lucina_jev=info,lucina_computer_use=info").init();
    let task = std::env::args().nth(1).expect("task");
    let model = lucina_llm::DeepSeek::new(lucina_llm::DeepSeekConfig::from_lookup(|k| std::env::var(k).ok())).expect("deepseek");
    lucina_computer_use::jev_drive::set_text_model(Arc::new(model));
    let jev = lucina_jev::shared();
    let only_jev = std::env::var("JEV_ONLY").is_ok();
    if only_jev {
        let target = lucina_computer_use::jev_drive::route(jev, &task).await;
        println!("route: {target:?}");
        let t = target.unwrap_or(lucina_computer_use::jev_drive::Where::Desktop);
        let out = lucina_computer_use::jev_drive::drive(jev, t, &task, &lucina_computer_use::Cancel::new()).await;
        println!("OUTCOME {out:#?}");
    } else {
        let out = lucina_computer_use::run_task("test:jev", &task, &lucina_computer_use::Cancel::new()).await;
        println!("RESULT {out:#?}");
    }
    println!("{}", jev.stats());
}
