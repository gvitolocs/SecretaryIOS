//! Lucina MCP OAuth 2.1: authorization code + PKCE S256, rotating refresh
//! tokens (`crates/secretary-mcp/src/oauth.rs`).
//!
//! Actors: the owner (signs in and consents on the authorize page), the
//! legitimate client (holds the PKCE verifier), and an attacker who may
//! learn a code (browser history, a log) or a refresh token, and may race
//! the client. The token endpoint's critical sections are split as in the
//! code, so concurrent requests interleave:
//!
//! - code: (1) look up under the `codes` lock (`redeemed` → replay), (2)
//!   client, redirect_uri, PKCE, resource checks, (3) set `redeemed` under
//!   the lock (already set → "code already used"), (4) `issue_tokens`
//!   writes the grant and the tokens to `oauth.json`;
//! - refresh: (1) `file.read` (used → replay → revoke), (2) `file.update`
//!   marks it used if still unused (else "already used"), (3) `issue_tokens`.

use stateright::{Model, Property};

use crate::{Case, Expect};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Who {
    Client,
    Attacker,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Req {
    /// A code redemption at step 1..=3; `grant` once chosen.
    Code { step: u8, grant: Option<u8> },
    /// A refresh with token `rt` at step 1..=2.
    Refresh { rt: u8, step: u8, grant: u8 },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Token {
    pub grant: u8,
    pub used: bool,
    /// Who knows it.
    pub holders: Vec<Who>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct State {
    /// The authorize request was approved after a successful owner login.
    pub consented: bool,
    pub login_ok: bool,
    pub code_issued: bool,
    /// The code reached the registered redirect_uri (the client) and maybe the attacker.
    pub code_holders: Vec<Who>,
    pub redeemed: Option<u8>,
    /// How many times the code was presented with a valid verifier context.
    pub code_presented: u8,
    /// Grants in oauth.json: revoked?
    pub grants: Vec<(u8, bool)>,
    pub next_grant: u8,
    pub refresh: Vec<Token>,
    /// Access tokens: (grant, holder).
    pub access: Vec<(u8, Who)>,
    pub requests: Vec<(Who, Req)>,
    /// Per refresh token: presented while already used or concurrently.
    pub rt_presentations: Vec<u8>,
    pub budget: u8,
    /// Tokens issued to a requester without the verifier.
    pub stolen_grant: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    OwnerLogin(bool),
    OwnerApprove,
    CodeLeaks,
    RefreshLeaks(u8),
    Redeem(Who),
    UseRefresh(Who, u8),
    Step(u8),
}

#[derive(Clone, Debug)]
pub struct OAuth {
    pub budget: u8,
    /// Fix O1: "mark redeemed" and `issue_tokens` happen in one critical
    /// section under the `codes` lock, so a replay never sees a redeemed
    /// code whose grant is not written yet; losing the "already used" race
    /// at commit time is a replay too (revoke).
    pub fix_code_replay: bool,
    /// Fix O2: losing the rotate race (`!rotated`) revokes the grant.
    pub fix_refresh_race: bool,
}

impl OAuth {
    fn revoke(s: &mut State, g: u8) {
        for gr in s.grants.iter_mut().filter(|x| x.0 == g) {
            gr.1 = true;
        }
        // revoke_grant: drop every token of the grant.
        s.refresh.retain(|t| t.grant != g);
        s.access.retain(|a| a.0 != g);
    }

    fn issue(s: &mut State, g: u8, who: Who, new_grant: bool) {
        if new_grant {
            s.grants.push((g, false));
        }
        s.refresh.push(Token {
            grant: g,
            used: false,
            holders: vec![who],
        });
        s.access.push((g, who));
    }

    fn revoked(s: &State, g: u8) -> bool {
        s.grants.iter().any(|x| x.0 == g && x.1)
    }

    fn all_done(s: &State) -> bool {
        s.requests.is_empty()
    }
}

impl Model for OAuth {
    type State = State;
    type Action = Action;

    fn init_states(&self) -> Vec<State> {
        vec![State {
            consented: false,
            login_ok: false,
            code_issued: false,
            code_holders: Vec::new(),
            redeemed: None,
            code_presented: 0,
            grants: Vec::new(),
            next_grant: 0,
            refresh: Vec::new(),
            access: Vec::new(),
            requests: Vec::new(),
            rt_presentations: Vec::new(),
            budget: self.budget,
            stolen_grant: false,
        }]
    }

    fn actions(&self, s: &State, a: &mut Vec<Action>) {
        if !s.login_ok && !s.code_issued {
            a.push(Action::OwnerLogin(true));
            a.push(Action::OwnerLogin(false));
        }
        if s.login_ok && !s.consented {
            a.push(Action::OwnerApprove);
        }
        if s.code_issued && !s.code_holders.contains(&Who::Attacker) {
            a.push(Action::CodeLeaks);
        }
        for (i, t) in s.refresh.iter().enumerate() {
            if !t.holders.contains(&Who::Attacker) {
                a.push(Action::RefreshLeaks(i as u8));
            }
        }
        if s.budget > 0 && s.requests.len() < 2 {
            for who in [Who::Client, Who::Attacker] {
                if s.code_holders.contains(&who) {
                    a.push(Action::Redeem(who));
                }
                for (i, t) in s.refresh.iter().enumerate() {
                    if t.holders.contains(&who) {
                        a.push(Action::UseRefresh(who, i as u8));
                    }
                }
            }
        }
        for i in 0..s.requests.len() {
            a.push(Action::Step(i as u8));
        }
    }

    fn next_state(&self, last: &State, action: Action) -> Option<State> {
        let mut s = last.clone();
        match action {
            Action::OwnerLogin(ok) => s.login_ok = ok,
            Action::OwnerApprove => {
                // authorize_post: approve + login ok → code to the pending
                // request's redirect_uri (validated against the client's list).
                s.consented = true;
                s.code_issued = true;
                s.code_holders = vec![Who::Client];
            }
            Action::CodeLeaks => s.code_holders.push(Who::Attacker),
            Action::RefreshLeaks(i) => s.refresh[i as usize].holders.push(Who::Attacker),
            Action::Redeem(who) => {
                s.budget -= 1;
                s.requests.push((
                    who,
                    Req::Code {
                        step: 1,
                        grant: None,
                    },
                ));
            }
            Action::UseRefresh(who, i) => {
                s.budget -= 1;
                let g = s.refresh[i as usize].grant;
                s.requests.push((
                    who,
                    Req::Refresh {
                        rt: i,
                        step: 1,
                        grant: g,
                    },
                ));
            }
            Action::Step(i) => {
                let (who, req) = s.requests[i as usize];
                let mut done = true;
                match req {
                    Req::Code { step: 1, .. } => {
                        s.code_presented += 1;
                        match s.redeemed {
                            // Replay: revoke_grant(g). Before the fix, a no-op if
                            // the grant is not in oauth.json yet.
                            Some(g) => OAuth::revoke(&mut s, g),
                            None => {
                                // (2) PKCE: only the client has the verifier; a
                                // failed verifier burns the code.
                                if who == Who::Attacker {
                                    s.code_holders.clear();
                                    s.code_issued = false;
                                } else {
                                    done = false;
                                    s.requests[i as usize].1 = Req::Code {
                                        step: 2,
                                        grant: None,
                                    };
                                }
                            }
                        }
                    }
                    Req::Code { step: 2, .. } if !s.code_issued => {
                        // Burned by a failed verifier meanwhile: "code already used".
                    }
                    Req::Code { step: 2, .. } => match s.redeemed {
                        // (3) set redeemed under the codes lock ...
                        None => {
                            let g = s.next_grant;
                            s.next_grant += 1;
                            s.redeemed = Some(g);
                            if self.fix_code_replay {
                                // ... and issue_tokens before the lock is released.
                                OAuth::issue(&mut s, g, who, true);
                                if who == Who::Attacker {
                                    s.stolen_grant = true;
                                }
                            } else {
                                done = false;
                                s.requests[i as usize].1 = Req::Code {
                                    step: 3,
                                    grant: Some(g),
                                };
                            }
                        }
                        // Lost the race: "code already used" (the fix revokes).
                        Some(g) => {
                            if self.fix_code_replay {
                                OAuth::revoke(&mut s, g);
                            }
                        }
                    },
                    Req::Code { grant: Some(g), .. } => {
                        // (4) issue_tokens: grant + tokens into oauth.json.
                        OAuth::issue(&mut s, g, who, true);
                        if who == Who::Attacker {
                            s.stolen_grant = true;
                        }
                    }
                    Req::Code { .. } => {}
                    Req::Refresh { rt, step: 1, grant } => {
                        let token = s.refresh.get(rt as usize).cloned();
                        match token {
                            Some(t) if t.grant == grant => {
                                if t.used {
                                    let n = &mut s.rt_presentations;
                                    n.push(rt);
                                    OAuth::revoke(&mut s, grant);
                                } else if !OAuth::revoked(&s, grant) {
                                    done = false;
                                    s.requests[i as usize].1 = Req::Refresh { rt, step: 2, grant };
                                }
                            }
                            _ => {} // missing (revoked): invalid_grant
                        }
                    }
                    Req::Refresh { rt, grant, .. } => {
                        // file.update: mark used if still unused, then issue.
                        match s.refresh.get_mut(rt as usize) {
                            Some(t) if t.grant == grant && !t.used => {
                                t.used = true;
                                OAuth::issue(&mut s, grant, who, false);
                            }
                            Some(t) if t.grant == grant => {
                                s.rt_presentations.push(rt);
                                if self.fix_refresh_race {
                                    OAuth::revoke(&mut s, grant);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                if done {
                    s.requests.remove(i as usize);
                }
            }
        }
        // Tokens are referenced by index: keep the list stable but bounded.
        Some(s)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always("code_used_once", |_, s: &State| s.grants.len() <= 1),
            Property::always("no_token_without_login_and_consent", |_, s: &State| {
                s.grants.is_empty() || (s.login_ok && s.consented)
            }),
            Property::always("verifier_bound", |_, s: &State| !s.stolen_grant),
            Property::always("code_replay_revokes_grant", |_, s: &State| {
                // Once every request finished: a code presented twice (or more)
                // leaves no live grant from it.
                !OAuth::all_done(s)
                    || s.code_presented < 2
                    || s.redeemed
                        .is_none_or(|g| OAuth::revoked(s, g) || !s.grants.iter().any(|x| x.0 == g))
            }),
            Property::always("refresh_reuse_revokes_grant", |_, s: &State| {
                !OAuth::all_done(s) || s.rt_presentations.is_empty() || s.grants.iter().all(|g| g.1)
            }),
            Property::sometimes("client_refreshed", |_, s: &State| {
                s.refresh.iter().filter(|t| t.used).count() >= 1 && s.grants.iter().any(|g| !g.1)
            }),
        ]
    }
}

pub const PROTOCOL: &str = "oauth";

pub fn cases(deep: bool) -> Vec<Case> {
    use Expect::*;
    let budget = if deep { 5 } else { 4 };
    let all = |code: Expect, refresh: Expect| {
        vec![
            ("code_used_once", Holds),
            ("no_token_without_login_and_consent", Holds),
            ("verifier_bound", Holds),
            ("code_replay_revokes_grant", code),
            ("refresh_reuse_revokes_grant", refresh),
            ("client_refreshed", Reached),
        ]
    };
    vec![
        Case::new(
            PROTOCOL,
            "O current main (code + refresh races)",
            OAuth {
                budget,
                fix_code_replay: false,
                fix_refresh_race: false,
            },
            None,
            all(Violated, Violated),
        ),
        Case::new(
            PROTOCOL,
            "O + fixes O1, O2 (verify-fixes)",
            OAuth {
                budget,
                fix_code_replay: true,
                fix_refresh_race: true,
            },
            None,
            all(Holds, Holds),
        ),
    ]
}
