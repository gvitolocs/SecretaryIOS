//! End-to-end test against a tiny in-process mock voice socket.
//!
//! The mock answers the section-10 handshake (`listening` with a `codec` key),
//! eats the frames until `end`, then sends `transcript`, `delta`, `answer`,
//! `audio` + one binary frame, `reply`, `done`. The recorded events must all
//! be present and ordered.

use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_tungstenite::{accept_async, tungstenite::Message};
use voice_probe::ws;

#[tokio::test]
async fn mock_turn_records_ordered_events() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/v1/voice/ws", listener.local_addr().unwrap());

    let audio_bytes = 9600u32; // 200 ms of 24 kHz s16le
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = accept_async(stream).await.unwrap();

        // start
        let m = ws.next().await.unwrap().unwrap();
        let v: serde_json::Value = serde_json::from_str(m.to_text().unwrap()).unwrap();
        assert_eq!(v["type"], "start");
        assert_eq!(v["mode"], "ptt");
        assert_eq!(v["purpose"], "command");
        assert_eq!(v["codec"], "pcm16");
        assert!(v.get("rate").is_none());
        assert!(v.get("block").is_none());
        ws.send(Message::text(json!({
            "type": "listening", "purpose": "command", "codec": "pcm16",
            "rate": 16000, "turn": "t1"
        }).to_string())).await.unwrap();

        // frames until end
        let mut frames = 0usize;
        loop {
            let m = ws.next().await.unwrap().unwrap();
            match m {
                Message::Binary(b) => {
                    frames += 1;
                    assert!(b.len() <= 8192);
                }
                Message::Text(t) => {
                    let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                    if v["type"] == "end" {
                        break;
                    }
                }
                other => panic!("unexpected {other:?}"),
            }
        }
        assert!(frames >= 2);

        tokio::time::sleep(Duration::from_millis(30)).await;
        ws.send(Message::text(json!({"type": "transcript", "text": "che ore sono"}).to_string()))
            .await
            .unwrap();
        ws.send(Message::text(json!({"type": "delta", "text": "Sono"}).to_string()))
            .await
            .unwrap();
        ws.send(Message::text(json!({"type": "answer", "text": "Sono le dieci.", "replaces": true}).to_string()))
            .await
            .unwrap();
        ws.send(Message::text(json!({"type": "audio", "rate": 24000}).to_string()))
            .await
            .unwrap();
        ws.send(Message::Binary(vec![0u8; audio_bytes as usize].into()))
            .await
            .unwrap();
        ws.send(Message::text(json!({"type": "reply", "text": "Sono le dieci."}).to_string()))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        ws.send(Message::text(json!({
            "type": "done",
            "ms": {"stt": 10, "lucina_first": 50, "tts_first": 80, "first_audio": 90, "total": 120}
        }).to_string()))
        .await
        .unwrap();
        // Drop the socket; the client already got `done`.
        frames
    });

    let (connect_ms, mut client) = ws::connect(&url, "probe-test-token").await.unwrap();
    let cfg = ws::WsConfig {
        purpose: "command".into(),
        codec: "pcm16".into(),
        adpcm_rate: None,
        adpcm_block: None,
        lang: Some("it".into()),
        thread: None,
        channel: None,
        source: Some("probe".into()),
        text: true,
        no_speak: false,
        frame_ms: 40,
        realtime: false, // the test must not pace for a second
        frames: vec![
            Message::Binary(vec![1u8; 640].into()),
            Message::Binary(vec![3u8; 640].into()),
            Message::Binary(vec![5u8; 100].into()),
        ],
        speech_ms: 200,
    };
    let (mut r, fatal) = ws::turn(&mut client, &cfg).await.unwrap();
    let frames = server.await.unwrap();
    assert!(!fatal);
    assert_eq!(frames, 3);
    r.connect_ms = Some(connect_ms); // main.rs records it on the first repeat

    // Connect was measured (loopback: quick).
    assert!(r.connect_ms.unwrap() < 10_000);

    // Section-10 server: `listening` echoed a `codec`.
    assert_eq!(r.server_section10, Some(true));

    // All the events are present and ordered from t0.
    let t = &r.t;
    for k in ["transcript", "first_delta", "answer", "first_audio", "reply", "done"] {
        assert!(t.get(k).is_some(), "missing {k}");
    }
    let (tr, fd, an, fa, rp, dn) = (
        t["transcript"],
        t["first_delta"],
        t["answer"],
        t["first_audio"],
        t["reply"],
        t["done"],
    );
    assert!(tr <= fd && fd <= an && an <= fa && fa <= rp && rp <= dn);
    assert!(dn > tr, "the 60 ms of sleeps must show in the order");
    assert_eq!(r.error, None);

    // The binary frame arrived after the `audio` header and was kept.
    assert_eq!(r.audio.len(), audio_bytes as usize);
    assert_eq!(r.audio_rate, 24000);
    assert_eq!(r.transcript_chars, "che ore sono".chars().count() as u32);
    assert_eq!(r.reply_chars, "Sono le dieci.".chars().count() as u32);
    assert!(r.up_bytes >= 3 * 2 + 15 + 15); // frames + start + end, at least
    assert!(r.down_bytes >= audio_bytes as u64);
    assert_eq!(r.server_ms.as_ref().unwrap()["total"], 120);

    // The JSON shape.
    let j = r.json();
    assert_eq!(j["mode"], "ws");
    assert_eq!(j["purpose"], "command");
    assert_eq!(j["codec"], "pcm16");
    assert_eq!(j["server_section10"], true);
    assert_eq!(j["t"]["done"], dn);
    assert!(j.get("error").is_none());
}
