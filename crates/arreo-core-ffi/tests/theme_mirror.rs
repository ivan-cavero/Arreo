//! The codec's theme mirror (T-0116).
//!
//! `WireMessage` mirrors `arreo_core::proto::Message` variant for variant, and the
//! match that builds it is exhaustive — so a new core variant is a *compile* error
//! in `codec.rs`, which is why this mirror cannot silently fall behind.
//!
//! What the compiler cannot catch is a mirror arm that maps a field wrongly or
//! drops a token. That is what this file asserts, so it lives beside the mirror
//! rather than inside the relay tests: the mirror is the codec's, not the
//! session's.

use arreo_core_ffi::codec::{
    codec_decode, codec_encode, codec_protocol_version, WireMessage, WireThemeTokens,
};
use arreo_core_ffi::theme::{FfiColor, FfiVariant, ThemeToken};

/// The theme pair round-trips through the mirror (T-0116's `Message::Theme` /
/// `ThemeReply`).
///
/// The codec's match over `Message` is exhaustive, so adding a variant to the core
/// is a *compile* error here — that tripwire is why this mirror cannot silently
/// fall behind. What it cannot catch is a mirror arm that maps a field wrongly, or
/// drops a token, which is what this asserts: encode → decode gives back what went
/// in, with every token present and its color spelled the theme file's way.
#[test]
fn the_theme_pair_round_trips_through_the_mirror() {
    let request = WireMessage::Theme {
        v: codec_protocol_version(),
        name: "pushed".to_string(),
        variant: FfiVariant::Light,
    };
    let body = codec_encode(request.clone()).expect("the request encodes");
    assert_eq!(codec_decode(body).expect("and decodes"), request);

    // The reply carries resolved tokens: literals only, no `defs` reference.
    let reply = WireMessage::ThemeReply {
        v: codec_protocol_version(),
        theme: WireThemeTokens {
            name: "pushed".to_string(),
            variant: FfiVariant::Dark,
            tokens: vec![
                ThemeToken {
                    name: "background".to_string(),
                    color: FfiColor::Rgb {
                        r: 0x12,
                        g: 0x14,
                        b: 0x18,
                    },
                },
                ThemeToken {
                    name: "question".to_string(),
                    color: FfiColor::Ansi { index: 10 },
                },
                ThemeToken {
                    name: "text".to_string(),
                    color: FfiColor::TerminalDefault,
                },
            ],
        },
    };
    let body = codec_encode(reply.clone()).expect("the reply encodes");
    let decoded = codec_decode(body).expect("and decodes");
    assert_eq!(decoded, reply, "every token survives, with its color");

    // And the values are the theme file's own spelling on the core side, which is
    // what makes the wire shape one document rather than two.
    let core = match decoded {
        WireMessage::ThemeReply { theme, .. } => theme,
        other => panic!("a theme reply decoded to {other:?}"),
    };
    let spelled: Vec<String> = core.tokens.iter().map(ThemeToken::color_as_text).collect();
    assert_eq!(spelled, vec!["#121418", "10", "none"]);
}
