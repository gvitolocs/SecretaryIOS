//! Differential test of `verification/verus/otp_owner.rs` against the real
//! `lucina_phone_core::reply::private_owner` (lucina-rs; unchanged by F1-F5).
//!
//! Belongs in `crates/lucina-phone-core/tests/verus_otp_owner.rs`; `diff/otp_run.sh`
//! runs it. The `/otp` dispatch kernel (`dispatch`, `secretary_chat_dispatch`) mirrors
//! lucina-server `respond` / `run` / `secretary_chat`, which need the relay and the
//! server crate; its one gate is `private_owner`, checked here exhaustively.

use lucina_phone_core::reply::{private_owner, Asker};

/// Plain-Rust copy of the kernel (`Surface`, `bytes_all_digits`, `private_owner_exec`).
mod kernel {
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub enum Surface {
        App,
        Telegram,
        Other,
    }

    pub fn surface(s: &str) -> Surface {
        match s {
            "app" => Surface::App,
            "telegram" => Surface::Telegram,
            _ => Surface::Other,
        }
    }

    pub struct Asker<'a> {
        pub surface: Surface,
        pub owner_authorized: bool,
        pub mcp: bool,
        pub owner_id: &'a [u8],
        pub user_id: &'a [u8],
        pub chat_id: &'a [u8],
        pub chat_private: bool,
        pub is_bot: bool,
    }

    fn all_digits(a: &[u8]) -> bool {
        a.iter().all(|b| (48..=57).contains(b))
    }

    pub fn private_owner_exec(a: &Asker) -> bool {
        let (app, telegram) = match a.surface {
            Surface::App => (true, false),
            Surface::Telegram => (false, true),
            Surface::Other => (false, false),
        };
        if app {
            return a.owner_authorized && !a.mcp;
        }
        telegram
            && !a.owner_id.is_empty()
            && all_digits(a.owner_id)
            && a.chat_private
            && a.user_id == a.owner_id
            && a.chat_id == a.owner_id
            && !a.is_bot
    }

    /// The spec (`private_owner` in the .rs kernel), written as the truth table.
    pub fn spec(a: &Asker) -> bool {
        (a.surface == Surface::App && a.owner_authorized && !a.mcp)
            || (a.surface == Surface::Telegram
                && !a.owner_id.is_empty()
                && all_digits(a.owner_id)
                && a.chat_private
                && a.user_id == a.owner_id
                && a.chat_id == a.owner_id
                && !a.is_bot)
    }
}

/// Every combination of the pools below: 6 surfaces x 5^3 ids x 4 chat types x 2^3 flags.
#[test]
fn kernel_matches_private_owner_exhaustively() {
    let surfaces = ["app", "telegram", "mcp", "", "App", "telegram "];
    let ids = ["42", "-42", "", "4a2", "43"];
    let chat_types = ["private", "group", "supergroup", ""];
    let (mut yes, mut total) = (0, 0);
    for surface in surfaces {
        for owner_id in ids {
            for user_id in ids {
                for chat_id in ids {
                    for chat_type in chat_types {
                        for bits in 0..8u8 {
                            let real = Asker {
                                surface,
                                owner_authorized: bits & 1 != 0,
                                mcp: bits & 2 != 0,
                                owner_id,
                                user_id,
                                chat_id,
                                chat_type,
                                is_bot: bits & 4 != 0,
                            };
                            let k = kernel::Asker {
                                surface: kernel::surface(surface),
                                owner_authorized: real.owner_authorized,
                                mcp: real.mcp,
                                owner_id: owner_id.as_bytes(),
                                user_id: user_id.as_bytes(),
                                chat_id: chat_id.as_bytes(),
                                chat_private: chat_type == "private",
                                is_bot: real.is_bot,
                            };
                            let got = private_owner(&real);
                            assert_eq!(got, kernel::private_owner_exec(&k), "{real:?}");
                            assert_eq!(got, kernel::spec(&k), "{real:?}");
                            if got {
                                assert!(!(surface == "app" && real.mcp), "{real:?}");
                                yes += 1;
                            }
                            total += 1;
                        }
                    }
                }
            }
        }
    }
    // Non-vacuity: both sides of the table are reached (app 4 x 500 combos; telegram "42" 4).
    assert!(yes > 0 && yes < total);
    eprintln!("private_owner == kernel on {total} askers, {yes} owner");
}

/// The header's observation: on Telegram, `mcp` does not change the answer.
#[test]
fn telegram_branch_ignores_mcp() {
    let a = Asker {
        surface: "telegram",
        owner_id: "42",
        user_id: "42",
        chat_id: "42",
        chat_type: "private",
        mcp: true,
        ..Default::default()
    };
    assert!(private_owner(&a));
}
