//! Pairing (T-0024): turning a short human code into a pinned device.
//!
//! One sentence: `arreo pair` shows four words; whoever types them proves they
//! can see the server's screen, and the SPAKE2 exchange turns those 32 bits
//! into an authenticated channel over which the device's certificate travels.
//!
//! Modules:
//! - [`code`] — the word list, the code, and its canonical password bytes.
//! - [`wire`] — the mailbox (four write-once slots) and its client.
//! - [`flow`] — the two sides' state machines and the MAC that confirms them.
//!
//! What this buys, exactly (ROADMAP §1 "pair in 30 seconds", §3.3, §4):
//! - **No password, no account, no SSH.** The code is the only shared secret.
//! - **No assumption that the network is friendly.** An active attacker who
//!   can read and inject every flight still needs the code; a wrong guess
//!   burns the session, so enumerating 2^32 codes is not a strategy. The relay
//!   sees opaque blobs and a session id.
//! - **Nothing half-done.** A failed pairing writes no key, no certificate and
//!   no pin — the caller asserts that by hashing the identity tree.
//!
//! The code never leaves the two devices: it is used as the SPAKE2 password and
//! nowhere else. That is why the relay's storage is not a second copy of the
//! secret, and why a relay compromise does not let an attacker pair.

pub mod code;
pub mod flow;
pub mod wire;

pub use code::{Code, WORDS, WORDS_PER_CODE};
pub use flow::{Invite, PairedDevice, PairingPhone, PairingServer, PhoneRequest};
pub use wire::{MailboxAddr, MailboxClient, MailboxRequest, MailboxResponse, Slot};

/// Everything that can go wrong between two humans and a pair of machines.
/// Typed because the CLI has to say something useful, and because "wrong code"
/// and "mailbox unreachable" must never look the same to the user.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PairingError {
    #[error("no OS entropy available: {0}")]
    Entropy(String),
    #[error("a code is {want} words, got {got}")]
    CodeShape { got: usize, want: usize },
    #[error("unknown code word {word:?}{}", hint_suffix(.hint))]
    UnknownWord {
        word: String,
        hint: Option<&'static str>,
    },
    #[error("malformed invite: {0}")]
    BadInvite(String),
    #[error("mailbox: {0}")]
    Mailbox(String),
    #[error("the pairing window closed while waiting for slot {waiting_for:?}")]
    Timeout { waiting_for: Slot },
    #[error("pairing exchange failed: {0}")]
    Spake(String),
    #[error("the code did not match — this pairing session is now burned")]
    CodeMismatch,
    #[error("the peer's confirmation did not verify")]
    Confirmation,
    #[error("the server's identity in the invite is malformed: {0}")]
    Identity(String),
    #[error("the paired certificate does not verify against this server")]
    Certificate,
}

fn hint_suffix(hint: &Option<&'static str>) -> String {
    match hint {
        Some(word) => format!(" (did you mean {word:?}?)"),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wrong_code_and_a_dead_mailbox_are_different_errors() {
        // The user-facing difference this type exists for: "you mistyped" must
        // not be reported as "the relay is down".
        let wrong = PairingError::CodeMismatch;
        let unreachable = PairingError::Mailbox("connection refused".into());
        assert_ne!(wrong, unreachable);
        assert!(wrong.to_string().contains("did not match"), "{wrong}");
        assert!(
            unreachable.to_string().contains("connection refused"),
            "{unreachable}"
        );
    }

    #[test]
    fn an_unknown_word_error_carries_its_hint() {
        let error = PairingError::UnknownWord {
            word: "bambu".into(),
            hint: Some("bamboo"),
        };
        assert_eq!(
            error.to_string(),
            "unknown code word \"bambu\" (did you mean \"bamboo\"?)"
        );
        let bare = PairingError::UnknownWord {
            word: "qqqq".into(),
            hint: None,
        };
        assert_eq!(bare.to_string(), "unknown code word \"qqqq\"");
    }
}
