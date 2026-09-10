//! T-0013 failing-first probes: MessagePack codec, version negotiation,
//! decoder robustness, and the 1 MB / 5 ms budget.
//!
//! Written before `proto::codec` exists — MUST fail to compile until it lands.

use arreo_core::proto::{codec, negotiate, Message, VERSION};

#[test]
fn hello_snapshot_delta_round_trip() {
    for message in [
        Message::Hello {
            v: VERSION,
            client: "arreo-cli-test".to_string(),
            wants: vec![VERSION],
        },
        Message::Snapshot {
            v: VERSION,
            id: "pane-1".to_string(),
            lines: vec!["hello".to_string(), "world".to_string()],
            cursor: (1, 5),
        },
        Message::Delta {
            v: VERSION,
            id: "pane-1".to_string(),
            from_line: 2,
            lines: vec!["next".to_string()],
        },
    ] {
        let bytes = codec::encode(&message).expect("encode");
        let back = codec::decode(&bytes).expect("decode");
        assert_eq!(message, back, "round-trip identical");
    }
}

#[test]
fn version_negotiation_rejects_incompatible() {
    // Same version: fine.
    assert!(negotiate(VERSION, &[VERSION]).is_ok());
    // Client offering only the future: loud rejection, never silent downgrade.
    assert!(negotiate(VERSION, &[VERSION + 1]).is_err());
    // Client offering nothing: loud rejection.
    assert!(negotiate(VERSION, &[]).is_err());
}

#[test]
fn decoder_rejects_garbage_without_panic() {
    for garbage in [
        vec![],
        vec![0xc1],
        vec![0xff, 0xff, 0xff],
        b"not msgpack at all................".to_vec(),
        vec![0x93, 0x01, 0x02],
    ] {
        assert!(
            codec::decode(&garbage).is_err(),
            "garbage rejected: {garbage:?}"
        );
    }
}

#[test]
fn one_mb_delta_within_budget() {
    // Timing policy: debug builds on a loaded box are noisy (single-shot
    // ranged 6–30 ms here), so this test asserts CORRECTNESS + size in debug
    // and reserves the < 5 ms timing gate for --release (proven green) and
    // the `bench --probe proto` harness below.
    let lines: Vec<String> = (0..1000).map(|i| format!("{:01024}", i)).collect();
    let message = Message::Delta {
        v: VERSION,
        id: "bulk".to_string(),
        from_line: 0,
        lines,
    };
    let start = std::time::Instant::now();
    let bytes = codec::encode(&message).expect("encode 1MB");
    let _encode_ms = start.elapsed();
    assert!(
        bytes.len() >= 1_000_000,
        "actually ~1MB (got {})",
        bytes.len()
    );
    let start = std::time::Instant::now();
    let back = codec::decode(&bytes).expect("decode 1MB");
    let _decode_ms = start.elapsed();
    assert_eq!(back, message, "1MB round-trip identical");
    // Timing gate: release-only (debug on a loaded box is noise — measured
    // 6–30 ms single-shot here vs green in release). CI debug runs prove
    // correctness; release + bench prove speed.
    #[cfg(not(debug_assertions))]
    {
        assert!(
            _encode_ms.as_millis() < 5,
            "encode 1MB < 5 ms (got {_encode_ms:?})"
        );
        assert!(
            _decode_ms.as_millis() < 5,
            "decode 1MB < 5 ms (got {_decode_ms:?})"
        );
    }
}

#[test]
fn frames_round_trip_over_a_stream_buffer() {
    use arreo_core::proto::{codec, Message, VERSION};
    let messages = vec![
        Message::Hello {
            v: VERSION,
            client: "a".to_string(),
            wants: vec![VERSION],
        },
        Message::Delta {
            v: VERSION,
            id: "p".to_string(),
            from_line: 0,
            lines: vec!["x".to_string()],
        },
        Message::Error {
            v: VERSION,
            message: "boom".to_string(),
        },
    ];
    // Concatenate frames like a stream socket would deliver them.
    let mut stream = Vec::new();
    for message in &messages {
        stream.extend_from_slice(&codec::encode_frame(message).expect("frame"));
    }
    // Decode back-to-back, advancing exactly.
    let mut rest = stream.as_slice();
    for want in &messages {
        let (got, consumed) = codec::decode_frame(rest).expect("deframe");
        assert_eq!(&got, want);
        rest = &rest[consumed..];
    }
    assert!(rest.is_empty());
    // Truncated frame = loud Truncated, never panic. (A lone partial frame:
    // decode_frame only ever reads the FIRST frame, so truncate frame 2 in
    // isolation — a short tail on a longer stream still yields frame 1.)
    let frame2 = codec::encode_frame(&messages[1]).expect("frame");
    assert!(codec::decode_frame(&frame2[..frame2.len() - 1]).is_err());
    assert!(codec::decode_frame(&frame2[..2]).is_err());
    assert!(codec::decode_frame(&[0x01]).is_err());
}

#[test]
fn state_event_and_metrics_round_trip() {
    use arreo_core::proto::{codec, AgentState, Message, VERSION};
    for message in [
        Message::StateEvent {
            v: VERSION,
            id: "p".to_string(),
            state: AgentState::Question,
            confidence: "inferred:silence+prompt-shape".to_string(),
            matched_pattern: Some("\\[y/n\\]".to_string()),
        },
        Message::Metrics {
            v: VERSION,
            id: "p".to_string(),
            rss_bytes: 1_234_567,
            cpu_percent: Some(12.5),
            pids: 3,
        },
        Message::Welcome {
            v: VERSION,
            server: "arreo-server-test".to_string(),
        },
        Message::Resume {
            v: VERSION,
            id: "p".to_string(),
            from_line: 42,
        },
    ] {
        let bytes = codec::encode(&message).expect("encode");
        assert_eq!(codec::decode(&bytes).expect("decode"), message);
    }
}

#[test]
fn control_verbs_round_trip_1_to_1_with_jsonl() {
    // The MessagePack control set covers every T-0005 JSONL verb so the
    // T-0014 cutover is mechanical, not a redesign.
    use arreo_core::proto::{codec, Message, VERSION};
    for message in [
        Message::Spawn {
            v: VERSION,
            id: "a".to_string(),
            program: "/bin/sh".to_string(),
            args: vec![],
            cols: 80,
            rows: 24,
            memory_max: None,
            pids_max: None,
            kill_on_breach: false,
        },
        Message::Panes {
            v: VERSION,
            panes: vec![],
        },
        Message::Attach {
            v: VERSION,
            id: "a".to_string(),
            from_line: 0,
        },
        Message::Send {
            v: VERSION,
            id: "a".to_string(),
            data: "hi".to_string(),
        },
        Message::Resize {
            v: VERSION,
            id: "a".to_string(),
            cols: 100,
            rows: 30,
        },
        Message::Kill {
            v: VERSION,
            id: "a".to_string(),
        },
        Message::Ok { v: VERSION },
        Message::Exited {
            v: VERSION,
            id: "a".to_string(),
            code: Some(0),
        },
    ] {
        let bytes = codec::encode(&message).expect("encode");
        assert_eq!(codec::decode(&bytes).expect("decode"), message);
    }
}

proptest::proptest! {
    /// Property: any generated message round-trips identically (the
    /// criterion's "any encoded message decodes identically").
    #[test]
    fn any_message_round_trips(
        id in "[a-z0-9-]{1,16}",
        line in "[ -~]{0,200}",
        nlines in 0usize..8,
        from_line in 0usize..10000,
        code in proptest::option::of(0u32..256),
    ) {
        use arreo_core::proto::{Message, VERSION, codec};
        let lines: Vec<String> = (0..nlines).map(|i| format!("{line}-{i}")).collect();
        let messages = [
            Message::Delta { v: VERSION, id: id.clone(), from_line, lines: lines.clone() },
            Message::Snapshot { v: VERSION, id: id.clone(), lines: lines.clone(), cursor: (0, 0) },
            Message::Send { v: VERSION, id: id.clone(), data: line.clone() },
            Message::Error { v: VERSION, message: line.clone() },
            Message::Exited { v: VERSION, id: id.clone(), code },
        ];
        for message in &messages {
            let bytes = codec::encode(message).expect("encode");
            proptest::prop_assert_eq!(&codec::decode(&bytes).expect("decode"), message);
        }
    }

    /// Property: decoder never panics on arbitrary bytes (fuzz-adjacent;
    /// the committed corpus in `proto_corpus_decode` covers regression).
    #[test]
    fn decoder_total_on_arbitrary_bytes(bytes in proptest::collection::vec(0u8..255, 0..256)) {
        use arreo_core::proto::codec;
        let _ = codec::decode(&bytes);
        let _ = codec::decode_frame(&bytes);
    }
}

/// Committed decoder corpus: garbage in = error out, pinned forever.
#[test]
fn proto_corpus_decode() {
    use arreo_core::proto::codec;
    let cases: &[&[u8]] = &[
        b"",
        b"\xc1",
        b"\xff\xff\xff",
        b"not msgpack at all................",
        &[0x93, 0x01, 0x02],
        &[0x81, 0xc0],
        &[0xdc, 0x00],
        b"\x93\x93\x93\x93\x93",
    ];
    for (i, case) in cases.iter().enumerate() {
        assert!(codec::decode(case).is_err(), "case {i} rejected");
    }
}
