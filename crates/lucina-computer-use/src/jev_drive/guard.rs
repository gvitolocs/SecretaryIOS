//! What Lucina may click, in code: the owner's signed-in sessions are his, so
//! anything that sends, pays, deletes or accepts is never offered to Jev, and a
//! shopping task stops at the checkout page and hands the payment to him.
//!
//! Two lists:
//! - [`HARD`]: never offered, whatever the task (paying, placing an order,
//!   sending, deleting, accepting terms, signing out, switching account, …);
//! - [`SOFT`]: words that are risky in general ("order", "checkout") but name
//!   the steps of filling a basket. In a shopping task a label that also
//!   matches [`SHOP_ALLOWED`] ("Add to order", "Go to checkout") is offered.
//!
//! A label that matches a hard phrase is risky even when it also looks like a
//! basket step ("Order and pay", "Add card").

/// Whether a drive may fill a basket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Kind {
    /// Read, search, navigate. No basket, no checkout.
    #[default]
    Browse,
    /// Fill a basket and reach the checkout page, then stop for the owner.
    Shopping,
}

/// Never offered (en, it, da). Matched as whole words or phrases.
const HARD: &[&str] = &[
    // Messages and accounts.
    "send", "invia", "sende", "reply", "rispondi", "svar", "forward", "inoltra", "videresend",
    "delete", "elimina", "cancella", "slet", "remove", "rimuovi", "fjern", "accept", "accetta",
    "acceptér", "accepter", "agree", "consent", "sign out", "log out", "logout", "esci", "log ud",
    "unsubscribe", "transfer", "trasferisci", "post", "pubblica", "publish", "submit", "invio",
    "archive", "archivia", "arkivér", "report spam", "block", "blocca", "install", "installa",
    "uninstall", "factory reset", "erase", "format",
    // Switching the signed-in account (seen on Google Takeout, 2026-10-28).
    "switch account", "cambia account", "skift konto", "add account", "aggiungi account",
    "tilføj konto", "add another account", "use another account",
    // Money: paying and placing orders.
    "pay", "paga", "pagare", "betal", "betaling", "payment", "pagamento", "buy", "køb", "compra",
    "acquista", "purchase", "bestil", "bestil nu", "bestil og betal", "place order",
    "place your order", "plaser ordre", "afgiv ordre", "order now", "ordina ora", "ordina e paga",
    "confirm order", "confirm and pay", "bekræft ordre", "bekræft og betal", "conferma ordine",
    "complete order", "submit order", "send order", "pay with", "betal med", "paga con",
    "apple pay", "google pay", "mobilepay", "paypal", "add card", "tilføj kort", "aggiungi carta",
    "card number", "kortnummer", "numero carta", "click to order", "klik for at bestille",
    "clicca per ordinare", "tryk for at bestille",
];

/// Risky unless the task is shopping and the label is a basket step.
const SOFT: &[&str] = &[
    "checkout", "order", "ordina", "ordine", "ordre", "kassen", "kasse", "cassa", "basket", "cart",
    "kurv", "kurven", "carrello",
];

/// Basket steps a shopping task may take (never paying: see [`HARD`]).
const SHOP_ALLOWED: &[&str] = &[
    "add to order", "add to basket", "add to cart", "add to bag", "add item", "tilføj til ordre",
    "tilføj til ordren", "tilføj til kurv", "tilføj til kurven", "læg i kurv", "læg i kurven",
    "aggiungi all'ordine", "aggiungi al carrello", "aggiungi all’ordine", "view order",
    "see order", "view basket", "view cart", "go to cart", "go to basket", "your order",
    "se ordre", "se kurv", "se din ordre", "din ordre", "vedi ordine", "il tuo ordine",
    "gå til kassen", "til kassen", "checkout", "go to checkout", "continue to checkout",
    "vai alla cassa", "vai al checkout",
];

/// Whole-word (or whole-phrase) match, case-insensitive.
fn has(label: &str, phrases: &[&str]) -> bool {
    let l = label.to_lowercase();
    phrases.iter().any(|w| {
        l.match_indices(w).any(|(i, _)| {
            let before = l[..i].chars().next_back();
            let after = l[i + w.len()..].chars().next();
            !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
        })
    })
}

/// Never offered, whatever the task.
pub fn hard_risky(label: &str) -> bool {
    has(label, HARD)
}

/// Whether Jev may be offered this label in a task of this kind.
pub fn risky(label: &str, kind: Kind) -> bool {
    if hard_risky(label) {
        return true;
    }
    if !has(label, SOFT) {
        return false;
    }
    !(kind == Kind::Shopping && has(label, SHOP_ALLOWED))
}

// ------------------------------------------------------------------ cookies

const CONSENT: &[&str] = &[
    "cookie", "cookies", "consent", "before you continue", "inden du fortsætter",
    "prima di continuare", "privacy settings", "vi bruger cookies", "usiamo i cookie",
    "we use cookies", "privatlivsindstillinger",
];

/// Buttons that decline non-essential cookies.
const DECLINE: &[&str] = &[
    "reject all", "reject", "decline", "decline all", "refuse all", "deny", "only necessary",
    "necessary only", "only essential", "essential only", "use necessary cookies only",
    "allow necessary only", "allow only necessary", "only necessary cookies",
    "reject optional cookies", "decline optional cookies", "continue without accepting",
    "afvis alle", "afvis", "afvis alle cookies", "kun nødvendige", "kun nødvendige cookies",
    "tillad kun nødvendige", "tillad nødvendige", "fortsæt uden at acceptere", "rifiuta tutto",
    "rifiuta", "rifiuta tutti", "solo necessari", "solo cookie necessari", "continua senza accettare",
];

/// The page shows a cookie or consent wall.
pub fn consent_wall(text: &str, labels: &[&str]) -> bool {
    has(text, CONSENT) || labels.iter().any(|l| has(l, CONSENT))
}

/// A button that declines non-essential cookies. "Accept only necessary" counts
/// (it declines the rest); "Accept all" never does.
pub fn declines_cookies(label: &str) -> bool {
    let l = label.trim().to_lowercase();
    if l.is_empty() || l.chars().count() > 60 {
        return false;
    }
    if has(&l, &["accept all", "accetta tutto", "accetta tutti", "accepter alle", "allow all", "tillad alle", "consenti tutti"]) {
        return false;
    }
    if DECLINE.iter().any(|d| l == *d || l.trim_end_matches(['.', '!']) == *d) {
        return true;
    }
    let necessary = has(&l, &["necessary", "nødvendige", "necessari", "essential", "essentials", "required only"]);
    let accept_or_allow = has(&l, &["accept", "accepter", "acceptér", "accetta", "allow", "tillad", "consenti", "use", "brug", "usa"]);
    necessary && (accept_or_allow || l.split_whitespace().count() <= 3)
}

// ------------------------------------------------------------------ checkout

/// Page words that say "this is the checkout".
const CHECKOUT: &[&str] = &[
    "checkout", "check out", "kassen", "kasse", "betaling", "payment", "pagamento", "cassa",
    "order summary", "ordreoversigt", "riepilogo ordine", "place order", "afgiv ordre",
];

/// Words before a total.
const TOTAL: &[&str] = &["at betale", "i alt", "totale", "total", "da pagare", "subtotal", "subtotale"];

/// The shopping drive has reached the checkout: the page says so and shows a
/// payment step (a button Lucina may never press, or a total to pay).
/// `hard_labels` are the labels the risky filter removed from this screen.
pub fn at_checkout(title: &str, url: &str, text: &str, hard_labels: &[String]) -> bool {
    let pay_button = hard_labels.iter().any(|l| {
        has(l, &[
            "pay", "betal", "paga", "place order", "place your order", "afgiv ordre", "plaser ordre",
            "bestil", "bestil nu", "bestil og betal", "order now", "ordina ora", "confirm order",
            "bekræft ordre", "conferma ordine", "complete order", "apple pay", "google pay",
            "mobilepay", "paypal", "card number", "kortnummer", "add card", "tilføj kort",
            "click to order", "klik for at bestille", "clicca per ordinare",
        ])
    });
    let says = has(title, CHECKOUT)
        || url.to_lowercase().contains("checkout")
        || text.lines().any(|l| l.starts_with('#') && has(l, CHECKOUT));
    pay_button || (says && total(text).is_some())
}

/// A sign-in wall in a shopping flow: signing in is the owner's.
pub fn login_wall(text: &str, has_password_field: bool) -> bool {
    has_password_field
        || has(text, &[
            "log in or sign up", "log in or create an account", "sign in to continue",
            "log ind eller opret", "log ind for at fortsætte", "accedi o registrati",
            "accedi per continuare", "continue with google", "fortsæt med google",
        ])
}

/// The amount to pay as the page writes it ("189,00 kr.", "DKK 189", "€ 12,50").
pub fn total(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    for marker in TOTAL {
        let mut from = 0;
        while let Some(i) = lower[from..].find(marker) {
            let at = from + i;
            from = at + marker.len();
            // "total" inside "subtotal" is the subtotal's turn, later in the list.
            if marker.starts_with("total") && lower[..at].ends_with("sub") {
                continue;
            }
            let end = lower.len().min(from + 80);
            let end = (end..=lower.len()).find(|e| lower.is_char_boundary(*e)).unwrap_or(lower.len());
            if let Some(m) = money(&text[from..end]) {
                return Some(m);
            }
        }
    }
    None
}

const CURRENCY: &[&str] = &["kr.", "kr", "dkk", "€", "eur", "$", "usd", "£", "gbp", "sek", "nok"];

/// The first amount with a currency next to it.
pub fn money(s: &str) -> Option<String> {
    let chars: Vec<(usize, char)> = s.char_indices().collect();
    let mut i = 0;
    while i < chars.len() {
        if !chars[i].1.is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = chars[i].0;
        let mut j = i;
        while j < chars.len()
            && (chars[j].1.is_ascii_digit()
                || (matches!(chars[j].1, '.' | ',' | '\u{a0}' | ' ')
                    && chars.get(j + 1).is_some_and(|c| c.1.is_ascii_digit())
                    && (chars[j].1 != ' ' || digits_after(&chars, j + 1) == 3)))
        {
            j += 1;
        }
        let end = chars.get(j).map(|c| c.0).unwrap_or(s.len());
        let before = s[..start].trim_end().to_lowercase();
        let after = s[end..].trim_start().to_lowercase();
        let cur_before = CURRENCY.iter().find(|c| before.ends_with(**c));
        let cur_after = CURRENCY.iter().find(|c| {
            after.starts_with(**c)
                && !after[c.len()..].chars().next().is_some_and(char::is_alphabetic)
        });
        if let Some(c) = cur_after {
            let tail_start = s[end..].len() - s[end..].trim_start().len();
            let tail = &s[end + tail_start..end + tail_start + c.len()];
            return Some(format!("{} {}", s[start..end].trim(), tail).trim().to_owned());
        }
        if let Some(c) = cur_before {
            let head = s[..start].trim_end();
            let head = &head[head.len() - c.len()..];
            return Some(format!("{head} {}", s[start..end].trim()));
        }
        i = j.max(i + 1);
    }
    None
}

fn digits_after(chars: &[(usize, char)], from: usize) -> usize {
    chars[from..].iter().take_while(|c| c.1.is_ascii_digit()).count()
}
