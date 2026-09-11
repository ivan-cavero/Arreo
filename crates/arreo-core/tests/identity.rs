//! T-0025 integration probes: device identity, hostile certificates, and the
//! total-verification property (ROADMAP §4: "claims point to artifacts").
//!
//! The unit tests in `src/identity/**` cover each rule; this file holds the
//! adversarial material a reviewer reads: a committed corpus of malformed
//! certificates (the regression net for the decoder) and proptests over
//! arbitrary bytes, because a certificate parser is attacker-facing input.

use arreo_core::identity::keys::{DeviceKey, RootKey};
use arreo_core::identity::role::Role;
use arreo_core::identity::{CertError, DeviceCert, DeviceId, DeviceIndex};

const NOW: i64 = 1_760_000_000_000;

fn cert(key: &DeviceKey, role: Role, serial: u64) -> DeviceCert {
    DeviceCert::issue(
        &RootKey::from_seed([42u8; 32]),
        &key.public(),
        "test-device",
        role,
        NOW,
        serial,
    )
}

/// Committed hostile corpus: every one of these must produce a typed error
/// (never a panic, never a trusted cert). Cases are msgpack-shaped on purpose —
/// a malformed blob that is *valid* msgpack is the interesting kind.
#[test]
fn hostile_cert_corpus_is_refused() {
    let cases: &[&[u8]] = &[
        b"",
        b"\xc1",
        b"not msgpack at all................",
        &[0xff, 0xff, 0xff, 0xff],
        &[0x81, 0xc0],
        &[0xdc, 0x00],
        // A map with the right shape but the wrong field types.
        &[
            0x82, 0xa7, b'p', b'a', b'y', b'l', b'o', b'a', b'd', 0xc0, 0xa9, b's', b'i', b'g',
            b'n', b'a', b't', b'u', b'r', b'e', 0xa1, b'x',
        ],
        // An array where a map is expected.
        &[0x92, 0x01, 0x02],
        // Deeply nested arrays (a decoder that recurses must still finish).
        &[0x91, 0x91, 0x91, 0x91, 0x91, 0x91, 0x91, 0x91, 0x91, 0x91],
    ];
    for (index, case) in cases.iter().enumerate() {
        match DeviceCert::decode(case) {
            Err(CertError::Decode(_)) => {}
            other => panic!("corpus case {index} ({case:?}) produced {other:?}"),
        }
    }
}

/// A structurally valid cert with a signature that is not 64 bytes must not
/// decode: the length is enforced, not truncated into place.
#[test]
fn a_cert_with_a_malformed_signature_length_is_refused() {
    use serde::Serialize;

    #[derive(Serialize)]
    struct Loose {
        payload: serde_json::Value,
        signature: Vec<u8>,
    }

    let key = DeviceKey::generate().expect("entropy");
    let good = cert(&key, Role::Viewer, 1);
    let payload = serde_json::to_value(&good.payload).expect("payload reflects to json");
    for len in [0usize, 1, 32, 63, 65, 128] {
        let loose = Loose {
            payload: payload.clone(),
            signature: vec![0u8; len],
        };
        let bytes = rmp_serde::to_vec_named(&loose).expect("encode");
        assert!(
            DeviceCert::decode(&bytes).is_err(),
            "a {len}-byte signature decoded into a certificate"
        );
    }
}

/// Rotation: a new keypair plus a new cert replaces the device, and the old
/// key can never open a session again (the acceptance criterion, at the
/// authorization layer the daemon uses).
#[test]
fn rotating_a_device_key_invalidates_the_old_one() {
    let root = RootKey::from_seed([42u8; 32]);
    let old = DeviceKey::generate().expect("entropy");
    let index = DeviceIndex::new();
    let mut index = index;
    index.insert(DeviceCert::issue(
        &root,
        &old.public(),
        "laptop",
        Role::Owner,
        NOW,
        1,
    ));
    index
        .authorize(&root.public(), &old.public())
        .expect("the original key is authorized");

    // Rotation: same human device, brand-new keypair and cert, moved over by
    // the explicit rotation call (a device id is a key fingerprint, so a new
    // key is a new id and only rotation can retire the old one).
    let new = DeviceKey::generate().expect("entropy");
    let old_id = DeviceId::from_key(&old.public());
    let new_id = index
        .rotate(
            &old_id,
            DeviceCert::issue(&root, &new.public(), "laptop", Role::Owner, NOW + 1_000, 2),
        )
        .expect("rotation");
    index
        .authorize(&root.public(), &new.public())
        .expect("the rotated key is authorized");
    // The old key's next connection is refused, and the refusal names the
    // device it became.
    match index.authorize(&root.public(), &old.public()) {
        Err(CertError::RotatedAway { replaced_by, .. }) => {
            assert_eq!(replaced_by, new_id.to_string());
        }
        other => panic!("the rotated-away key was not refused: {other:?}"),
    }
    // The old cert (still on disk) does not rescue it.
    let stale = DeviceCert::issue(&root, &old.public(), "laptop", Role::Owner, NOW, 1);
    stale
        .verify(&root.public(), &old.public())
        .expect("the old cert is still cryptographically valid");
    assert!(
        index.authorize(&root.public(), &old.public()).is_err(),
        "the index, not the cert file, decides"
    );
}

/// A viewer's certificate authorizes observing and refuses driving, checked at
/// the policy layer the daemon calls before it acts.
#[test]
fn a_viewer_certificate_cannot_drive_agents() {
    use arreo_core::identity::role::{check, Verb};
    let root = RootKey::from_seed([42u8; 32]);
    let viewer = DeviceKey::generate().expect("entropy");
    let mut index = DeviceIndex::new();
    index.insert(DeviceCert::issue(
        &root,
        &viewer.public(),
        "phone",
        Role::Viewer,
        NOW,
        1,
    ));
    let record = index
        .authorize(&root.public(), &viewer.public())
        .expect("authorized");
    assert_eq!(record.role, Role::Viewer);
    check(record.role, Verb::Read).expect("a viewer may read");
    check(record.role, Verb::Attach).expect("a viewer may attach");
    assert!(
        check(record.role, Verb::Send).is_err(),
        "a viewer may not send"
    );
    assert!(
        check(record.role, Verb::Spawn).is_err(),
        "a viewer may not spawn"
    );
}

/// A certificate issued by any other root key is worthless, even if every
/// field is otherwise perfect — the pin is the root, not the shape.
#[test]
fn only_the_pinned_root_can_issue() {
    let attacker_root = RootKey::from_seed([1u8; 32]);
    let pinned_root = RootKey::from_seed([42u8; 32]);
    let victim = DeviceKey::generate().expect("entropy");
    let forged = DeviceCert::issue(
        &attacker_root,
        &victim.public(),
        "laptop",
        Role::Owner,
        NOW,
        99,
    );
    let mut index = DeviceIndex::new();
    index.insert(forged);
    assert_eq!(
        index.authorize(&pinned_root.public(), &victim.public()),
        Err(CertError::BadSignature),
        "a forged cert must not become authority"
    );
}

proptest::proptest! {
    /// A certificate decoder is attacker-facing input: arbitrary bytes must
    /// never panic, and anything that decodes must be re-encodable.
    #[test]
    fn cert_decoding_is_total(bytes in proptest::collection::vec(0u8..=255, 0..512)) {
        if let Ok(cert) = DeviceCert::decode(&bytes) {
            let reencoded = cert.encode().expect("a decoded cert re-encodes");
            let again = DeviceCert::decode(&reencoded).expect("and decodes again");
            proptest::prop_assert_eq!(again, cert);
        }
    }

    /// Verification is total over arbitrary signature bytes and arbitrary keys:
    /// no panic, and a cert that was not issued for the presented key never
    /// verifies.
    #[test]
    fn verification_never_accepts_a_foreign_key(
        seed in proptest::collection::vec(0u8..=255, 32..=32),
        signature in proptest::collection::vec(0u8..=255, 64..=64),
    ) {
        let root = RootKey::from_seed([42u8; 32]);
        let mine = DeviceKey::generate().expect("entropy");
        let theirs = DeviceKey::generate().expect("entropy");
        let mut seed_bytes = [0u8; 32];
        seed_bytes.copy_from_slice(&seed);
        let cert_key = DeviceKey::from_seed(seed_bytes);
        if cert_key.public() == mine.public() || cert_key.public() == theirs.public() {
            return Ok(());
        }
        let mut sig = [0u8; 64];
        sig.copy_from_slice(&signature);
        let forged = DeviceCert {
            payload: cert(&mine, Role::Owner, 1).payload,
            signature: sig,
        };
        // Whatever this is, it must not authorize the unrelated key.
        proptest::prop_assert!(forged.verify(&root.public(), &theirs.public()).is_err());
        let _ = cert_key;
    }

    /// Device ids parse or fail; there is no third outcome.
    #[test]
    fn device_id_parsing_is_total(text in "[ -~]{0,40}") {
        match DeviceId::parse(&text) {
            Ok(id) => proptest::prop_assert_eq!(id.as_str().len(), 32),
            Err(CertError::BadDeviceId(_)) => {}
            Err(other) => proptest::prop_assert!(false, "unexpected error {:?}", other),
        }
    }
}
