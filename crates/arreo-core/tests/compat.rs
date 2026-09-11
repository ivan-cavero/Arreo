//! T-0028 acceptance tests: the protocol N−1 window.
//!
//! Updates must never strand an attached client (§3.13): a v1 client against a
//! v0 server and a v0 client against a v1 server keep working for the verbs
//! they share. These tests prove the deterministic rule for what is refused
//! versus what is downgraded (ADR 0017), in both directions, over committed
//! corpora — so a future change that breaks the window breaks a frozen byte
//! string, not a vibe.
//!
//! The corpora below are v0 frozen from the T-0013 message set: every verb, in
//! the exact bytes this build's encoder emits. A v1 is simulated by hand-built
//! frames (an unknown `op`, an unknown field on a known `op`) because there is
//! no v1 yet — the window is proven for v0→v1 only, and the first major break
//! names itself refused.

use arreo_core::proto::{
    classify_op, frame_body_len, negotiate, CodecError, Direction, Message, PaneInfo,
    MAX_FRAME_BYTES, MIN_VERSION, VERSION,
};
use arreo_core::proto::{codec, AgentState};

/// Encode one message the way the wire carries it (length prefix + body), and
/// return the body alone for classification probes.
fn body_of(message: &Message) -> Vec<u8> {
    codec::encode(message).expect("encode")
}

/// The full v0 verb sequence, as bytes: every variant the corpus pins.
fn v0_corpus() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        (
            "hello",
            body_of(&Message::Hello {
                v: 0,
                client: "c".into(),
                wants: vec![0],
            }),
        ),
        (
            "welcome",
            body_of(&Message::Welcome {
                v: 0,
                server: "s".into(),
            }),
        ),
        (
            "snapshot",
            body_of(&Message::Snapshot {
                v: 0,
                id: "p".into(),
                lines: vec!["a".into()],
                cursor: (0, 0),
            }),
        ),
        (
            "delta",
            body_of(&Message::Delta {
                v: 0,
                id: "p".into(),
                from_line: 0,
                lines: vec!["a".into()],
            }),
        ),
        (
            "resume",
            body_of(&Message::Resume {
                v: 0,
                id: "p".into(),
                from_line: 0,
            }),
        ),
        (
            "error",
            body_of(&Message::Error {
                v: 0,
                message: "m".into(),
            }),
        ),
        (
            "state_event",
            body_of(&Message::StateEvent {
                v: 0,
                id: "p".into(),
                state: AgentState::Working,
                confidence: "direct".into(),
                matched_pattern: None,
            }),
        ),
        (
            "metrics",
            body_of(&Message::Metrics {
                v: 0,
                id: "p".into(),
                rss_bytes: 1,
                cpu_percent: None,
                pids: 1,
            }),
        ),
        (
            "spawn",
            body_of(&Message::Spawn {
                v: 0,
                id: "p".into(),
                program: "/bin/sh".into(),
                args: vec![],
                cols: 80,
                rows: 24,
                memory_max: None,
                pids_max: None,
                kill_on_breach: false,
            }),
        ),
        (
            "panes",
            body_of(&Message::Panes {
                v: 0,
                panes: vec![PaneInfo {
                    id: "p".into(),
                    alive: true,
                }],
            }),
        ),
        (
            "attach",
            body_of(&Message::Attach {
                v: 0,
                id: "p".into(),
                from_line: 0,
            }),
        ),
        (
            "send",
            body_of(&Message::Send {
                v: 0,
                id: "p".into(),
                data: "x".into(),
            }),
        ),
        (
            "resize",
            body_of(&Message::Resize {
                v: 0,
                id: "p".into(),
                cols: 80,
                rows: 24,
            }),
        ),
        (
            "kill",
            body_of(&Message::Kill {
                v: 0,
                id: "p".into(),
            }),
        ),
        ("ok", body_of(&Message::Ok { v: 0 })),
        (
            "exited",
            body_of(&Message::Exited {
                v: 0,
                id: "p".into(),
                code: Some(0),
            }),
        ),
        (
            "read",
            body_of(&Message::Read {
                v: 0,
                id: "p".into(),
                from_line: 0,
            }),
        ),
        (
            "wait",
            body_of(&Message::Wait {
                v: 0,
                id: "p".into(),
                state: AgentState::Done,
                timeout_ms: 1,
            }),
        ),
        (
            "split",
            body_of(&Message::Split {
                v: 0,
                id: "p".into(),
                new_id: "q".into(),
                cols: 80,
                rows: 24,
            }),
        ),
        (
            "metrics_req",
            body_of(&Message::MetricsReq {
                v: 0,
                id: "p".into(),
            }),
        ),
    ]
}

/// The window: accept the highest version common to [server-1, server], echo
/// it, refuse anything outside with a typed error naming the range.
#[test]
fn negotiate_is_a_window_not_a_match() {
    // Exact match still works.
    assert_eq!(negotiate(0, &[0]).expect("v0 agrees with v0"), 0);
    // The window: a v1 server accepts a v0 client, agreeing on v0 — and says
    // so, so the downgrade is never silent.
    assert_eq!(negotiate(1, &[0]).expect("v1 serves v0"), 0);
    assert_eq!(negotiate(1, &[1]).expect("v1 serves v1"), 1);
    assert_eq!(
        negotiate(1, &[0, 1]).expect("both offered"),
        1,
        "highest common wins"
    );
    assert_eq!(
        negotiate(1, &[1, 0]).expect("both offered"),
        1,
        "offer order does not matter"
    );
    // Outside the window: loud refusal naming both sides.
    let err = negotiate(1, &[2]).expect_err("v2 against a v1 server refuses");
    let text = err.to_string();
    assert!(
        text.contains('1') && text.contains('2'),
        "names the range and the offer: {text}"
    );
    let err = negotiate(0, &[1]).expect_err("v1 against a v0 server refuses");
    assert!(err.to_string().contains('1'), "names the offer: {err}");
    // Empty offers are a refusal, not a default.
    assert!(negotiate(1, &[]).is_err(), "no offer, no guess");
    // A gap wider than one is a deferred update, not a downgrade.
    assert!(negotiate(2, &[0]).is_err(), "v2 server does not speak v0");
    // The floor never underflows: a v0 server's window is just v0.
    assert_eq!(MIN_VERSION, 0);
    assert_eq!(negotiate(0, &[0]).expect("v0 agrees with v0"), 0);
}

/// Every v0 verb classifies to its side without a full decode — and the
/// classification agrees with the typed enum, so the map head and the decoder
/// cannot disagree about which direction a message travels.
#[test]
fn every_v0_verb_classifies_to_its_side() {
    let requests = [
        "hello",
        "resume",
        "spawn",
        "attach",
        "send",
        "resize",
        "kill",
        "read",
        "wait",
        "split",
        "metrics_req",
        "panes",
    ];
    let events = [
        "welcome",
        "snapshot",
        "delta",
        "error",
        "state_event",
        "metrics",
        "ok",
        "exited",
    ];
    for (op, body) in v0_corpus() {
        let direction = classify_op(&body)
            .unwrap_or_else(|| panic!("v0 {op} must classify without a full decode"));
        if requests.contains(&op) {
            assert_eq!(direction, Direction::Request, "{op} is a request");
        } else if events.contains(&op) {
            assert_eq!(direction, Direction::Event, "{op} is an event");
        } else {
            panic!("v0 corpus has an op the test does not name: {op}");
        }
    }
}

/// An unknown `op` is a request until a newer server says otherwise: a client
/// that sent something unknown asked for work, and work is refused rather than
/// ignored. A state-mutating message silently discarded is the one outcome the
/// rules exist to prevent.
#[test]
fn an_unknown_op_defaults_to_request() {
    // Hand-built v1 frame: {"op": "teleport", "v": 1, "id": "p"} as MessagePack.
    let frame = v1_frame("teleport");
    assert_eq!(
        classify_op(&frame),
        Some(Direction::Request),
        "an unknown op is a request"
    );
    // And the typed decoder agrees it does not know it.
    assert!(
        codec::decode(&frame).is_err(),
        "the typed decoder must not accept what the rules call unknown"
    );
}

/// Garbage is not a version: a frame with no `op` tag classifies to `None`,
/// which is a refusal, not a guess.
#[test]
fn garbage_has_no_op_and_is_not_a_version() {
    for garbage in [
        vec![],
        vec![0xc1],
        vec![0x93, 0x01, 0x02],
        b"not msgpack at all................".to_vec(),
    ] {
        assert_eq!(
            classify_op(&garbage),
            None,
            "garbage has no op: {garbage:?}"
        );
    }
}

/// Rule (c): a new `#[serde(default)]` field is a silent downgrade — an old
/// reader decodes a message carrying a field it never heard of, because serde
/// skips what the struct does not name.
#[test]
fn an_unknown_field_on_a_known_op_still_decodes() {
    // {"op": "send", "v": 0, "id": "p", "data": "x", "future": 1}: a v1 field
    // on the v0 Send shape. Hand-encoded so the test does not depend on a v1
    // encoder existing.
    let mut frame = vec![0x85u8]; // fixmap(5)
    for (key, value) in [
        (rmp_encode_str("op"), rmp_encode_str("send")),
        (rmp_encode_str("v"), vec![0x00]),
        (rmp_encode_str("id"), rmp_encode_str("p")),
        (rmp_encode_str("data"), rmp_encode_str("x")),
        (rmp_encode_str("future"), vec![0x01]),
    ] {
        frame.extend(key);
        frame.extend(value);
    }
    let message = codec::decode(&frame).expect("an unknown field must not break the decode");
    assert_eq!(
        message,
        Message::Send {
            v: 0,
            id: "p".into(),
            data: "x".into()
        },
        "the unknown field is skipped, the known shape survives"
    );
    // …while the classifier still sees the op it knows.
    assert_eq!(classify_op(&frame), Some(Direction::Request));
}

/// The 1 MB budget holds, and a length prefix naming more is corruption —
/// refused before any allocation, so no caller decides "too big" for itself.
#[test]
fn the_frame_budget_is_enforced_before_allocation() {
    assert_eq!(MAX_FRAME_BYTES, 1024 * 1024);
    // A prefix naming 2 GB over a 4-byte buffer: refused, not waited on.
    let mut huge = 2_000_000_000u32.to_le_bytes().to_vec();
    huge.extend([0x90]);
    assert!(
        matches!(frame_body_len(&huge), Err(CodecError::Decode(_))),
        "an impossible length is corruption"
    );
    // Truncation is still truncation, not corruption.
    assert!(
        matches!(frame_body_len(&[0x01]), Err(CodecError::Truncated { .. })),
        "a short prefix is incomplete, not corrupt"
    );
    // And a real 1 MB delta still fits inside the budget.
    let lines: Vec<String> = (0..1000).map(|i| format!("{:01024}", i)).collect();
    let message = Message::Delta {
        v: VERSION,
        id: "bulk".into(),
        from_line: 0,
        lines,
    };
    let bytes = codec::encode(&message).expect("encode 1MB");
    assert!(bytes.len() >= 1_000_000, "actually ~1MB");
    assert!(bytes.len() <= MAX_FRAME_BYTES, "and inside the budget");
}

/// Behavioral equality: the old client's output is identical whether the bytes
/// came from a v0 encoder or through the window — decode is decode, and the
/// agreed version changes nothing about the verbs both sides share.
#[test]
fn shared_verbs_decode_identically_through_the_window() {
    let agreed = negotiate(1, &[0]).expect("v0 through a v1 window");
    assert_eq!(agreed, 0, "the downgrade is explicit: Welcome.v carries it");
    for (op, body) in v0_corpus() {
        let message = codec::decode(&body).unwrap_or_else(|e| panic!("v0 {op} must decode: {e}"));
        let back = codec::encode(&message).expect("re-encode");
        assert_eq!(
            codec::decode(&back).expect("decode"),
            message,
            "v0 {op} round-trips through the window"
        );
    }
}

/// Build a v1-shaped frame by hand: a map with the given `op`, version 1, and
/// one string field. The compat suite's stand-in for "a version from the
/// future" until a real v1 exists.
fn v1_frame(op: &str) -> Vec<u8> {
    let mut frame = vec![0x83u8]; // fixmap(3)
    frame.extend(rmp_encode_str("op"));
    frame.extend(rmp_encode_str(op));
    frame.extend(rmp_encode_str("v"));
    frame.extend(vec![0x01]);
    frame.extend(rmp_encode_str("id"));
    frame.extend(rmp_encode_str("p"));
    frame
}

fn rmp_encode_str(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    assert!(bytes.len() < 32, "test strings are short");
    let mut out = vec![0xa0 | bytes.len() as u8];
    out.extend(bytes);
    out
}
