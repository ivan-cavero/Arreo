//! Chaos probe 7 (deterministic fuzz): VT parser + state engine over a
//! seeded corpus — no `cargo-fuzz` dep, reproducible forever.
//!
//! A xorshift PRNG (fixed seed) generates hostile inputs: random bytes biased
//! toward ESC/C0/UTF-8-boundary shapes, random splits (partial feeds), random
//! alt-screen toggles. Invariants after every input: no panic (we're alive),
//! VT RAM bounded, engine state always a valid variant, log pageable.

use arreo_core::state::{Adapter, Engine};
use arreo_core::vt::VtPane;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn hostile_byte(rng: &mut Rng) -> u8 {
    match rng.below(10) {
        0 => 0x1b,
        1 => rng.below(0x20) as u8,
        2 => 0x07,
        3 => 0x9b,
        4 => match rng.below(4) {
            0 => 0xc3,
            1 => 0xe2,
            2 => 0xf0,
            _ => 0x28,
        },
        5 => b'[',
        6 => b'm',
        7 => b'\n',
        8 => rng.below(128) as u8 + 32,
        _ => rng.next() as u8,
    }
}

pub fn run() -> Result<String, String> {
    const ITERS: usize = 4000;
    const SEED: u64 = 0x5EED_C0DE;
    let mut rng = Rng(SEED);
    let mut vt = VtPane::new(80, 24);
    let mut engine = Engine::new(Adapter::default(), 0);
    let mut t = 0u64;
    let mut total_bytes = 0usize;
    for i in 0..ITERS {
        let len = 1 + rng.below(64) as usize;
        let mut chunk = Vec::with_capacity(len);
        for _ in 0..len {
            chunk.push(hostile_byte(&mut rng));
        }
        // Occasionally inject a full alt-screen toggle or a clean line so
        // the parser must resync between hostile bursts.
        if i % 97 == 0 {
            chunk.extend_from_slice(b"\x1b[?1049hVIM\x1b[?1049l");
        }
        if i % 131 == 0 {
            chunk.extend_from_slice(b"clean-sync-line\n");
        }
        vt.feed(&chunk);
        total_bytes += chunk.len();
        for event in engine.feed(&chunk, t) {
            let _ = event;
        }
        t += rng.below(3000);
        for event in engine.tick(t) {
            let _ = event;
        }
        if vt.ram_bytes() > 3 * 1024 * 1024 {
            return Err(format!("iter {i}: vt ram blown"));
        }
        let _ = engine.state();
    }
    vt.spill_to_disk().map_err(|e| format!("spill: {e}"))?;
    let total = vt.total_lines();
    if total > 0 {
        let _ = vt.page(0, 1).map_err(|e| format!("page head: {e}"))?;
        let _ = vt
            .page(total - 1, 1)
            .map_err(|e| format!("page tail: {e}"))?;
    }
    Ok(format!(
        "deterministic fuzz: {ITERS} iters, {total_bytes} bytes, seed {SEED:#x}, no panic, bounded"
    ))
}
