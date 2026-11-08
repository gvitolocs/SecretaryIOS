//! Grant verification, single use, pending reuse, deny and expiry, against
//! the in-process mock approvals server.

use std::sync::Arc;
use std::time::Duration;

use lucina_approvals::client::{NewApproval, RequesterInfo};
use lucina_approvals::grant::{self, Expect, GrantError, PubKey};
use lucina_approvals::testing::{Fixture, GrantSpec, Mock};
use lucina_approvals::{now_unix, plan_hash, ApprovalError, Approvals, Outcome};
use serde_json::json;

fn request(action: &str, hash: &str) -> NewApproval {
    NewApproval {
        requester: RequesterInfo {
            kind: "lucina".into(),
            name: "Lucina".into(),
            detail: "test".into(),
        },
        action: action.into(),
        summary: "s".into(),
        details: "d".into(),
        risk: "high".into(),
        sensitive: true,
        payload_hash: hash.into(),
        ttl_secs: 300,
    }
}

fn expect<'a>(action: &'a str, hash: &'a str, id: Option<&'a str>) -> Expect<'a> {
    Expect {
        action,
        payload_hash: hash,
        id,
        require_biometric: true,
        now: now_unix(),
    }
}

fn good_payload(mock_exp_in: i64, action: &str, hash: &str) -> serde_json::Value {
    let now = now_unix();
    json!({"v":1,"id":"apr_x","action":action,"requester":{"kind":"lucina","name":"Lucina"},
           "payloadHash":hash,"decision":"approved","decidedBy":"dev","biometric":true,
           "iat":now,"exp":now+mock_exp_in})
}

#[tokio::test]
async fn grant_checks_signature_exp_action_and_payload_hash() {
    let mock = Mock::start().await;
    let dir = tempfile::tempdir().unwrap();
    mock.write_pubkey_raw(&dir.path().join("k"));
    let key = PubKey::load(&dir.path().join("k")).unwrap();
    let h = "sha256:aaaa";

    let g = mock.sign(&good_payload(120, "root.exec", h));
    let claims = grant::verify(&key, &g, &expect("root.exec", h, Some("apr_x"))).unwrap();
    assert_eq!(claims.id, "apr_x");
    assert!(claims.biometric);

    // wrong action / payload / id
    assert!(matches!(
        grant::verify(&key, &g, &expect("secret.reveal", h, None)),
        Err(GrantError::WrongAction { .. })
    ));
    assert_eq!(
        grant::verify(&key, &g, &expect("root.exec", "sha256:bbbb", None)).unwrap_err(),
        GrantError::WrongPayload
    );
    assert_eq!(
        grant::verify(&key, &g, &expect("root.exec", h, Some("apr_other"))).unwrap_err(),
        GrantError::WrongId
    );
    // expired
    let old = mock.sign(&good_payload(-1, "root.exec", h));
    assert_eq!(
        grant::verify(&key, &old, &expect("root.exec", h, None)).unwrap_err(),
        GrantError::Expired
    );
    // not an approval
    let mut p = good_payload(120, "root.exec", h);
    p["decision"] = json!("denied");
    assert_eq!(
        grant::verify(&key, &mock.sign(&p), &expect("root.exec", h, None)).unwrap_err(),
        GrantError::NotApproved
    );
    // no biometric proof on a sensitive action
    let mut p = good_payload(120, "root.exec", h);
    p["biometric"] = json!(false);
    assert_eq!(
        grant::verify(&key, &mock.sign(&p), &expect("root.exec", h, None)).unwrap_err(),
        GrantError::NotBiometric
    );
    // a signature by another key, and a tampered payload under the real signature
    let other = Mock::start().await;
    let forged = other.sign(&good_payload(120, "root.exec", h));
    assert_eq!(
        grant::verify(&key, &forged, &expect("root.exec", h, None)).unwrap_err(),
        GrantError::BadSignature
    );
    let (payload, sig) = g.split_once('.').unwrap();
    let tampered_payload = {
        use base64::Engine;
        let mut v: serde_json::Value = serde_json::from_slice(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(payload)
                .unwrap(),
        )
        .unwrap();
        v["payloadHash"] = json!("sha256:cccc");
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(&v).unwrap())
    };
    assert_eq!(
        grant::verify(
            &key,
            &format!("{tampered_payload}.{sig}"),
            &expect("root.exec", "sha256:cccc", None)
        )
        .unwrap_err(),
        GrantError::BadSignature
    );
}

#[tokio::test]
async fn approved_grant_is_spent_once() {
    let fx = Fixture::new().await;
    let a = Arc::new(fx.approvals());
    let req = request("secret.reveal", "sha256:11");
    let begun = a.begin(&req).await.unwrap();
    assert!(!begun.reused);
    fx.mock.approve(&begun.id);
    let out = a
        .finish(&req, &begun, Duration::from_secs(5))
        .await
        .unwrap();
    let Outcome::Approved(c) = out else {
        panic!("{out:?}")
    };
    assert_eq!(c.id, begun.id);
    // the same approval, polled again: the server hands out the same grant, the registry refuses it
    let again = a.finish(&req, &begun, Duration::from_secs(5)).await;
    assert_eq!(again, Err(ApprovalError::Grant(GrantError::Replay)));
    // and so does a second process sharing the used-ids file
    let b = fx.approvals();
    assert_eq!(
        b.finish(&req, &begun, Duration::from_secs(5)).await,
        Err(ApprovalError::Grant(GrantError::Replay))
    );
}

#[tokio::test]
async fn bad_grants_are_refused_and_not_spent() {
    for spec in [
        GrantSpec {
            wrong_key: true,
            ..Default::default()
        },
        GrantSpec {
            exp_in: -5,
            ..Default::default()
        },
        GrantSpec {
            action: Some("root.exec".into()),
            ..Default::default()
        },
        GrantSpec {
            payload_hash: Some("sha256:ffff".into()),
            ..Default::default()
        },
        GrantSpec {
            biometric: false,
            ..Default::default()
        },
        GrantSpec {
            id: Some("apr_someone_else".into()),
            ..Default::default()
        },
    ] {
        let fx = Fixture::new().await;
        let a = fx.approvals();
        let req = request("secret.reveal", "sha256:22");
        let begun = a.begin(&req).await.unwrap();
        fx.mock.approve_with(&begun.id, &spec);
        let out = a.finish(&req, &begun, Duration::from_secs(5)).await;
        assert!(
            matches!(out, Err(ApprovalError::Grant(_))),
            "{spec:?}: {out:?}"
        );
        assert!(!std::fs::read_to_string(&fx.cfg.used_file)
            .unwrap_or_default()
            .contains(&begun.id));
    }
}

#[tokio::test]
async fn deny_and_expiry_end_the_request() {
    let fx = Fixture::new().await;
    let a = fx.approvals();
    let req = request("secret.reveal", "sha256:33");
    let b1 = a.begin(&req).await.unwrap();
    fx.mock.deny(&b1.id);
    match a.finish(&req, &b1, Duration::from_secs(5)).await.unwrap() {
        Outcome::Denied { id, decided_by } => {
            assert_eq!(id, b1.id);
            assert_eq!(decided_by.as_deref(), Some("iPhone"));
        }
        o => panic!("{o:?}"),
    }
    // a denied request is not reused: the next ask creates a new one
    let b2 = a.begin(&req).await.unwrap();
    assert!(!b2.reused && b2.id != b1.id);
    fx.mock.expire(&b2.id);
    assert_eq!(
        a.finish(&req, &b2, Duration::from_secs(5)).await.unwrap(),
        Outcome::Expired { id: b2.id.clone() }
    );
}

#[tokio::test]
async fn pending_request_is_reused_by_a_follow_up() {
    let fx = Fixture::new().await;
    let a = fx.approvals();
    let req = request("secret.reveal", "sha256:44");
    let first = a.ask(&req, Duration::from_millis(300)).await.unwrap();
    let Outcome::Pending { id, .. } = first else {
        panic!("{first:?}")
    };
    let second = a.ask(&req, Duration::from_millis(300)).await.unwrap();
    assert!(
        matches!(second, Outcome::Pending { id: ref again, .. } if *again == id),
        "{second:?}"
    );
    assert_eq!(fx.mock.created().len(), 1, "no second approval was created");
    // a different payload is a different thing
    let other = request("secret.reveal", "sha256:45");
    assert!(matches!(
        a.ask(&other, Duration::from_millis(300)).await.unwrap(),
        Outcome::Pending { .. }
    ));
    assert_eq!(fx.mock.created().len(), 2);
    // the owner approves; the follow-up gets the grant, still one request for it
    fx.mock.approve(&id);
    let third = a.ask(&req, Duration::from_secs(5)).await.unwrap();
    assert!(
        matches!(third, Outcome::Approved(ref c) if c.id == id),
        "{third:?}"
    );
    assert_eq!(fx.mock.created().len(), 2);
}

#[tokio::test]
async fn reused_request_whose_grant_went_stale_is_replaced() {
    let fx = Fixture::new().await;
    let a = fx.approvals();
    let req = request("secret.reveal", "sha256:55");
    let Outcome::Pending { id, .. } = a.ask(&req, Duration::from_millis(300)).await.unwrap() else {
        panic!()
    };
    // approved, but nobody asked until the 120 s grant had run out
    fx.mock.approve_with(
        &id,
        &GrantSpec {
            exp_in: -5,
            ..Default::default()
        },
    );
    let out = a.ask(&req, Duration::from_millis(300)).await.unwrap();
    assert!(
        matches!(out, Outcome::Pending { id: ref new, .. } if *new != id),
        "{out:?}"
    );
    assert_eq!(fx.mock.created().len(), 2);
}

#[tokio::test]
async fn not_configured_or_unreachable_fails_closed() {
    // no pinned key
    let fx = Fixture::new().await;
    let mut cfg = fx.cfg.clone();
    cfg.pubkey_file = fx.dir.path().join("missing.pub");
    assert!(matches!(
        Approvals::new(cfg),
        Err(ApprovalError::NotConfigured(_))
    ));
    // no server configured
    assert!(matches!(
        Approvals::new(lucina_approvals::ApprovalsConfig::default()),
        Err(ApprovalError::NotConfigured(_))
    ));
    // a server without the endpoint (or failing)
    let a = fx.approvals();
    fx.mock.fail_create_with(404);
    let req = request("secret.reveal", &plan_hash(&[vec!["x".into()]]));
    assert!(a.ask(&req, Duration::from_millis(200)).await.is_err());
    fx.mock.fail_create_with(500);
    assert!(matches!(
        a.ask(&req, Duration::from_millis(200)).await,
        Err(ApprovalError::Server { status: 500, .. })
    ));
}
