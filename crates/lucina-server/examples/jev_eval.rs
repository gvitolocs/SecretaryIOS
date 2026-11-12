//! Live check of Jev intent routing on typical owner messages (needs JEV_API_KEY).
//! `cargo run -p lucina-server --example jev_eval`
use lucina_server::jev_route::classify_intent;
use lucina_server::planner::TurnInput;

const CASES: &[(&str, &str)] = &[
    ("ciao Lucina!", "social_greeting"),
    (
        "ricordami di comprare il latte domani alle 9",
        "schedule_reminder",
    ),
    ("ricordami domani alle 9 di pagare", "schedule_reminder"),
    // 2026-10-30: the owner's answer to her question about his Mentor meeting.
    ("Mentor session 6 november at 12:30", "calendar_event"),
    ("lezione di Security giovedì alle 10", "calendar_event"),
    ("che promemoria ho oggi?", "list_reminders"),
    (
        "aggiungi uova e pane alla lista della spesa",
        "grocery.shopping_list.add",
    ),
    (
        "cosa c'è nella lista della spesa?",
        "grocery.shopping_list.query",
    ),
    (
        "ho comprato 2 litri di latte e un chilo di pasta",
        "grocery.purchase",
    ),
    (
        "ci sono offerte sul caffè da Netto?",
        "grocery.offer.search",
    ),
    (
        "riassumimi questa pagina https://example.com/article",
        "web_crawl",
    ),
    (
        "controlla la mia Outlook e dimmi se ho mail nuove dal professore",
        "computer_task",
    ),
    (
        "apri l'app Impostazioni sul tuo telefono Android e dimmi la versione",
        "computer_task",
    ),
    ("quanto devono bollire i fusilli?", "chat"),
    (
        "ti ricordi cosa avevamo deciso per il viaggio a Roma?",
        "explicit_memory_recall",
    ),
    ("disegnami un gatto in stile acquerello", "generate_image"),
    ("a che ora parte il mio volo per CPH?", "chat"),
    ("che modello stai usando adesso?", "model_status"),
    (
        "fai fare a Codex il fix del bug nel parser di pokoin",
        "cursor_task",
    ),
    ("what's the weather like tomorrow in Aarhus?", "chat"),
    (
        "quanto ho speso in spesa questo mese?",
        "grocery.expense.query",
    ),
];

#[tokio::main]
async fn main() {
    let jev = lucina_jev::shared();
    assert!(jev.configured(), "JEV_API_KEY missing");
    let mut ok = 0;
    for (msg, want) in CASES {
        let t = TurnInput {
            user_text: msg,
            memory: "",
            user_label: "Giuseppe",
            platform: "telegram",
            device: "",
        };
        let started = std::time::Instant::now();
        match classify_intent(jev, &t).await {
            Some((v, c)) => {
                let got = v["intent"].as_str().unwrap_or("");
                let hit = got == *want;
                ok += hit as usize;
                println!(
                    "{} {:>4}ms conf={c:.2} got={got:<28} want={want:<28} mode={} tools={} | {msg}",
                    if hit { "OK " } else { "BAD" },
                    started.elapsed().as_millis(),
                    v["reply_mode"],
                    v["needs_tools"]
                );
            }
            None => println!("ERR {msg}: {}", jev.stats()["lastError"]),
        }
    }
    println!("accuracy {ok}/{}", CASES.len());
    println!("{}", jev.stats());
}
