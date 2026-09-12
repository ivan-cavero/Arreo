//! `arreo attach --machine <name>`: reach another machine's panes by name
//! (T-0045, ROADMAP §3.7).
//!
//! One sentence: a name resolves through the account's directory to *which key*
//! to dial, and then the same client a local attach uses (T-0015) speaks the same
//! protocol over the relay — no SSH, no IPs, no ports, and nothing dialed from
//! argv.
//!
//! ## What a name has to resolve to, and why
//!
//! The directory is keyed by **machine id**, which is a machine's *root* key
//! (`MachineId::from_key`, T-0043) — an identity, not a route. Reaching a machine
//! needs the key its **daemon** authenticates with, because:
//!
//! - the relay moves bytes between **device ids**, and a machine's daemon
//!   authenticates as a device (the one pairing created for it) — a different key
//!   from the machine's root key;
//! - the Noise handshake proves the peer **holds the key we pinned** (ADR 0011),
//!   so a client needs the key itself: an id cannot be turned into one.
//!
//! Both come from the row's `daemon_key` (T-0045), which the relay writes from the
//! certificate that authenticated the session asserting the row — a machine cannot
//! advertise a route it does not hold. The alternative, letting the relay say who
//! is at the other end, is what ADR 0011 forbids.

use crate::ExitCode;
use arreo_core::mesh::resolve::{by_name, ResolveError};
use arreo_core::mesh::session::{Client, Target};
use arreo_core::proto::{Message, VERSION};
use std::path::PathBuf;

/// Exit codes, one vocabulary with `arreo machines`.
const OK: u8 = 0;
const USAGE: u8 = 2;
const UNKNOWN_MACHINE: u8 = 3;
const UNREACHABLE: u8 = 4;
const CONFLICT: u8 = 5;

/// Which path to a machine to try (T-0045's `--link`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkMode {
    /// Use whatever is available; today that is the relay.
    Auto,
    /// The relay, explicitly.
    Relay,
    /// Not implemented: LAN discovery (mDNS for a known `machine_id`) is its own
    /// piece of work, and the directory carries no addresses.
    LanDirect,
}

impl LinkMode {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "auto" => Some(Self::Auto),
            "relay" => Some(Self::Relay),
            "lan-direct" => Some(Self::LanDirect),
            _ => None,
        }
    }
}

/// Which exit code a resolution failure deserves: `arreo machines`' one
/// vocabulary, so an operator learns it once. The kind is the resolver's, not this
/// file's — a caller that maps them must not also be the one deciding them.
pub fn exit_code(e: &ResolveError) -> u8 {
    match e {
        ResolveError::Usage(_) => USAGE,
        ResolveError::UnknownMachine(_) => UNKNOWN_MACHINE,
        ResolveError::Unreachable(_) => UNREACHABLE,
    }
}

/// The only pane on a machine, or a message naming the choices.
pub async fn only_pane(target: &Target) -> Result<String, (u8, String)> {
    let mut client = Client::connect_to(target)
        .await
        .map_err(|e| (UNREACHABLE, e.to_string()))?;
    client
        .send(&Message::Panes {
            v: VERSION,
            panes: Vec::new(),
        })
        .await
        .map_err(|e| (UNREACHABLE, e.to_string()))?;
    match client.recv().await {
        Ok(Message::Panes { panes, .. }) => match panes.len() {
            0 => Err((UNKNOWN_MACHINE, "it has no panes".to_string())),
            1 => Ok(panes[0].id.clone()),
            n => Err((
                USAGE,
                format!(
                    "it has {n} panes; name one: {}",
                    panes
                        .iter()
                        .map(|pane| pane.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )),
        },
        Ok(Message::Error { message, .. }) => Err((refusal_code(&message), message)),
        Ok(other) => Err((UNREACHABLE, format!("expected a pane list, got {other:?}"))),
        Err(e) => Err((UNREACHABLE, e.to_string())),
    }
}

/// The exit code a peer's refusal deserves.
///
/// The trust ledger's refusals (T-0046) are `CONFLICT`, the same code
/// `arreo machines trust` uses, so an operator learns one vocabulary; anything
/// else is a reachability problem. The test is on the *shape* of the refusal the
/// ledger produces (it always ends by naming the command), which is why the
/// matching is by that phrase rather than by a code — the wire carries a sentence.
fn refusal_code(message: &str) -> u8 {
    if message.contains("arreo machines trust") {
        CONFLICT
    } else {
        UNREACHABLE
    }
}

/// Attach to a resolved target and stream its pane.
///
/// The same client a local attach uses, so "the same protocol, no SSH, no IPs, no
/// ports" is the code path rather than a promise.
pub async fn run(resolved: Target, pane: &str, machine: &str) -> ExitCode {
    let mut client = match Client::connect_to(&resolved).await {
        Ok(client) => client,
        Err(e) => {
            eprintln!("attach: {machine}: {e}");
            return ExitCode::from(UNREACHABLE);
        }
    };
    if let Err(e) = client
        .send(&Message::Attach {
            v: VERSION,
            id: pane.to_string(),
            from_line: 0,
        })
        .await
    {
        eprintln!("attach: {machine}: {e}");
        return ExitCode::from(UNREACHABLE);
    }
    loop {
        match client.recv().await {
            Ok(Message::Snapshot { lines, .. }) | Ok(Message::Delta { lines, .. }) => {
                for text in lines {
                    println!("{text}");
                }
            }
            Ok(Message::Exited { code, .. }) => {
                eprintln!("attach: pane exited (code {code:?})");
                return ExitCode::from(OK);
            }
            Ok(Message::Error { message, .. }) => {
                // The refusal is the peer's — including the trust ledger's, whose
                // message names the machine and the exact granting command. Passed
                // through unchanged: rewording it here would lose that.
                eprintln!("attach: {machine}: {message}");
                return ExitCode::from(refusal_code(&message));
            }
            Ok(_) => {}
            Err(e) => {
                eprintln!("attach: {machine}: {e}");
                return ExitCode::from(UNREACHABLE);
            }
        }
    }
}

/// Parse `attach`'s flags. Separate from the verb so the argument handling is
/// testable without a relay.
pub struct AttachArgs {
    pub machine: String,
    pub pane: Option<String>,
    pub link: LinkMode,
    pub config: Option<PathBuf>,
}

pub fn parse_args(kept: &[String]) -> Result<AttachArgs, (u8, String)> {
    let mut machine: Option<String> = None;
    let mut pane: Option<String> = None;
    let mut link = LinkMode::Auto;
    let mut config: Option<PathBuf> = None;
    let mut i = 0;
    while i < kept.len() {
        match kept[i].as_str() {
            "--machine" => match kept.get(i + 1) {
                Some(value) if !value.starts_with('-') => {
                    machine = Some(value.clone());
                    i += 2;
                    continue;
                }
                _ => {
                    return Err((
                        USAGE,
                        "--machine needs a machine name (see `arreo machines list`)".to_string(),
                    ))
                }
            },
            "--link" => match kept.get(i + 1).map(|value| LinkMode::parse(value)) {
                Some(Some(mode)) => {
                    link = mode;
                    i += 2;
                    continue;
                }
                _ => return Err((USAGE, "--link takes auto, relay or lan-direct".to_string())),
            },
            "--config" => match kept.get(i + 1) {
                Some(value) => {
                    config = Some(PathBuf::from(value));
                    i += 2;
                    continue;
                }
                None => return Err((USAGE, "--config needs a path".to_string())),
            },
            other if !other.starts_with('-') && pane.is_none() => pane = Some(other.to_string()),
            other => {
                return Err((USAGE, format!("unexpected argument {other:?}")));
            }
        }
        i += 1;
    }
    let Some(machine) = machine else {
        return Err((USAGE, "attach: --machine needs a machine name".to_string()));
    };
    Ok(AttachArgs {
        machine,
        pane,
        link,
        config,
    })
}

/// The verb: resolve, pick a pane, attach.
pub async fn cmd_attach(kept: &[String]) -> ExitCode {
    let args = match parse_args(kept) {
        Ok(args) => args,
        Err((code, message)) => {
            eprintln!("{message}");
            return ExitCode::from(code);
        }
    };
    if args.link == LinkMode::LanDirect {
        // Honest rather than silently dialing the relay: LAN discovery is its own
        // piece of work, and a `--link` value that quietly means something else is
        // worse than one that refuses.
        eprintln!(
            "attach: --link lan-direct is not implemented (LAN discovery is a separate task; the \
             directory carries no addresses). Use --link relay or --link auto."
        );
        return ExitCode::from(USAGE);
    }
    let resolved = match by_name(&args.machine, args.config.as_deref()).await {
        Ok(resolved) => resolved,
        Err(e) => {
            eprintln!("attach: {}", e.message());
            return ExitCode::from(exit_code(&e));
        }
    };
    let pane = match args.pane {
        Some(pane) => pane,
        None => match only_pane(&resolved.target).await {
            Ok(pane) => pane,
            Err((code, message)) => {
                eprintln!("attach: {}: {message}", resolved.name);
                return ExitCode::from(code);
            }
        },
    };
    // Taken apart before the move: `run` consumes the target, and the note is
    // built from what is left.
    let note = resolved.directory_note();
    let outcome = run(resolved.target, &pane, &resolved.name).await;
    if outcome != ExitCode::from(OK) {
        // The failure is over; what the *directory* thought is the other half an
        // operator needs, and by now it is a fact rather than a prediction.
        eprintln!("attach: {note}");
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|a| a.to_string()).collect()
    }

    /// The flags an operator types, and the mistakes: reported in the terms they
    /// used, before anything is dialed.
    #[test]
    fn attach_arguments_are_parsed_and_refused_before_any_dial() {
        let parsed = parse_args(&args(&["--machine", "workbox"])).expect("parses");
        assert_eq!(parsed.machine, "workbox");
        assert_eq!(parsed.pane, None);
        assert_eq!(parsed.link, LinkMode::Auto);

        let parsed =
            parse_args(&args(&["--machine", "pi", "pane-7", "--link", "relay"])).expect("parses");
        assert_eq!(parsed.pane.as_deref(), Some("pane-7"));
        assert_eq!(parsed.link, LinkMode::Relay);

        // A missing value for --machine must not silently consume nothing.
        assert!(parse_args(&args(&["--machine"])).is_err());
        assert!(parse_args(&args(&["--machine", "--link"])).is_err());
        // An unknown link is refused rather than defaulted: silently using the
        // relay when the operator asked for LAN is the wrong kind of quiet.
        assert!(parse_args(&args(&["--machine", "pi", "--link", "carrier-pigeon"])).is_err());
        // No --machine at all.
        assert!(parse_args(&args(&["pane-7"])).is_err());
    }

    /// A trust refusal and a reachability failure are different codes: the first
    /// tells the operator to run a command, the second that something is down.
    #[test]
    fn a_trust_refusal_has_its_own_exit_code() {
        let ledger_refusal = "machine workbox has no grant for this device, so Read is refused. \
                              Grant it with: arreo machines trust dev_1 --machine workbox \
                              --role viewer --yes";
        assert_eq!(refusal_code(ledger_refusal), CONFLICT);
        assert_eq!(refusal_code("connection reset"), UNREACHABLE);
        assert_eq!(refusal_code("unknown request panes"), UNREACHABLE);
        assert_eq!(OK, 0, "and the success code is distinct from all of them");
    }
}
