//! The battle's single source of randomness.
//!
//! # Determinism contract
//!
//! Given the same [`Format`](crate::Format), teams, [`Seed`] and sequence of
//! submitted choices, a battle produces bit-identical state and events on
//! every platform. Mechanics code must therefore draw *all* randomness from
//! the battle's [`BattleRng`], use integer arithmetic only, and never iterate
//! a hash map.
//!
//! # Backends
//!
//! Derived operations ([`BattleRng::random`], [`BattleRng::chance`],
//! [`BattleRng::shuffle`]) follow Pokémon Showdown's `PRNG` formulas exactly.
//! Only the raw 32-bit source differs:
//!
//! * [`Seed::Fast`]: xoshiro256++ — the default, for training throughput.
//! * [`Seed::ShowdownGen5`]: Showdown's Gen 5 LCG. Together with the engine
//!   consuming randomness in the same order as Showdown, this lets the
//!   conformance suite compare logs against the real simulator line by line.
//! * [`Seed::Sodium`]: Showdown's default `sodium` PRNG (`SodiumRNG` in
//!   `sim/prng.ts`): each draw is a ChaCha20 block keyed by the seed, whose
//!   first 32 bytes become the next seed. Battles Showdown seeds
//!   itself (`PRNG.generateSeed`) use it, so replaying their seeds needs it.
//!
//! The backend is a small enum rather than a type parameter so `Battle`
//! stays a single concrete type for bindings; the branch is perfectly
//! predictable in a hot loop.

/// A battle seed: with the format, the teams and the choices, it decides
/// everything (see the module's determinism contract), so it is what a
/// battle is replayed from ([`Battle::seed`](crate::Battle::seed)).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Seed {
    /// Seed for the fast xoshiro256++ backend (expanded with SplitMix64).
    Fast(u64),
    /// Showdown Gen 5 LCG seed: four 16-bit words, most significant first,
    /// as in Showdown's `[a, b, c, d]` seed arrays.
    ShowdownGen5([u16; 4]),
    /// Showdown's `sodium` seed: the 32-byte ChaCha20 key its `SodiumRNG`
    /// reads from the hex after `sodium,` (`Utils.bufWriteHex` of the hex
    /// padded to 64 digits). Reading the seed string is the caller's job
    /// (`pikapro-server` does it as Showdown does).
    Sodium([u8; 32]),
}

impl Seed {
    /// A seed for the fast backend: [`Seed::Fast`].
    pub const fn fast(seed: u64) -> Seed {
        Seed::Fast(seed)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Backend {
    Xoshiro([u64; 4]),
    Gen5(u64),
    Sodium([u8; 32]),
}

/// Deterministic RNG. `Clone` produces an identical, independent stream,
/// which is what tree search wants when it clones a battle.
///
/// Equality and hashing consider only the generator state, not the trace.
#[derive(Clone, Debug)]
pub struct BattleRng {
    backend: Backend,
    /// Every `(bound, result)` drawn since the last [`take_trace`], when
    /// tracing is enabled. A bound of 0 stands for 2^32 (a raw draw).
    ///
    /// [`take_trace`]: BattleRng::take_trace
    trace: Option<Vec<(u32, u32)>>,
}

impl PartialEq for BattleRng {
    fn eq(&self, other: &Self) -> bool {
        self.backend == other.backend
    }
}

impl Eq for BattleRng {}

impl core::hash::Hash for BattleRng {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.backend.hash(state);
    }
}

const GEN5_MUL: u64 = 0x5D58_8B65_6C07_8965;
const GEN5_ADD: u64 = 0x0026_9EC3;

fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// The nonce of Showdown's `SodiumRNG`, "compatible with libsodium's
/// `randombytes_buf_deterministic`".
const SODIUM_NONCE: &[u8; 12] = b"LibsodiumDRG";

/// Showdown's `SodiumRNG.next()`: encrypts 36 zero bytes with ChaCha20
/// (`ts-chacha20`: the IETF variant, block counter 0) keyed by the seed.
/// The first 32 bytes are the next seed; the next 4, big-endian, the draw.
fn sodium_next(key: &mut [u8; 32]) -> u32 {
    let block = chacha20_block(key, 0, SODIUM_NONCE);
    key.copy_from_slice(&block[..32]);
    u32::from_be_bytes([block[32], block[33], block[34], block[35]])
}

/// One ChaCha20 block (RFC 8439, section 2.3): 20 rounds over the
/// constants, the key, the counter and the nonce as little-endian words.
fn chacha20_block(key: &[u8; 32], counter: u32, nonce: &[u8; 12]) -> [u8; 64] {
    fn quarter(x: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
        x[a] = x[a].wrapping_add(x[b]);
        x[d] = (x[d] ^ x[a]).rotate_left(16);
        x[c] = x[c].wrapping_add(x[d]);
        x[b] = (x[b] ^ x[c]).rotate_left(12);
        x[a] = x[a].wrapping_add(x[b]);
        x[d] = (x[d] ^ x[a]).rotate_left(8);
        x[c] = x[c].wrapping_add(x[d]);
        x[b] = (x[b] ^ x[c]).rotate_left(7);
    }
    let word = |b: &[u8], i: usize| u32::from_le_bytes([b[4 * i], b[4 * i + 1], b[4 * i + 2], b[4 * i + 3]]);
    let mut input = [0u32; 16];
    input[..4].copy_from_slice(&[0x6170_7865, 0x3320_646e, 0x7962_2d32, 0x6b20_6574]);
    for i in 0..8 {
        input[4 + i] = word(key, i);
    }
    input[12] = counter;
    for i in 0..3 {
        input[13 + i] = word(nonce, i);
    }
    let mut x = input;
    for _ in 0..10 {
        quarter(&mut x, 0, 4, 8, 12);
        quarter(&mut x, 1, 5, 9, 13);
        quarter(&mut x, 2, 6, 10, 14);
        quarter(&mut x, 3, 7, 11, 15);
        quarter(&mut x, 0, 5, 10, 15);
        quarter(&mut x, 1, 6, 11, 12);
        quarter(&mut x, 2, 7, 8, 13);
        quarter(&mut x, 3, 4, 9, 14);
    }
    let mut out = [0u8; 64];
    for i in 0..16 {
        out[4 * i..4 * i + 4].copy_from_slice(&x[i].wrapping_add(input[i]).to_le_bytes());
    }
    out
}

impl BattleRng {
    /// A generator whose stream `seed` decides (its backend is the seed's
    /// kind). A battle makes its own from
    /// [`BattleOptions::seed`](crate::BattleOptions::seed); make one to
    /// drive a policy ([`LegalActions::sample`](crate::LegalActions::sample))
    /// without touching the battle's stream.
    pub fn new(seed: Seed) -> BattleRng {
        let backend = match seed {
            Seed::Fast(s) => {
                let mut sm = s;
                Backend::Xoshiro([splitmix64(&mut sm), splitmix64(&mut sm), splitmix64(&mut sm), splitmix64(&mut sm)])
            }
            Seed::ShowdownGen5([a, b, c, d]) => {
                Backend::Gen5(((a as u64) << 48) | ((b as u64) << 32) | ((c as u64) << 16) | d as u64)
            }
            Seed::Sodium(key) => Backend::Sodium(key),
        };
        BattleRng { backend, trace: None }
    }

    /// Starts recording every draw (see [`take_trace`](Self::take_trace)).
    /// For conformance debugging; costs a push per draw.
    pub(crate) fn enable_trace(&mut self) {
        self.trace.get_or_insert_with(Vec::new);
    }

    /// Whether draws are being recorded ([`enable_trace`](Self::enable_trace)).
    #[inline]
    pub(crate) fn is_tracing(&self) -> bool {
        self.trace.is_some()
    }

    /// Removes and returns the draws recorded since the last call. Empty when
    /// tracing is off.
    pub(crate) fn take_trace(&mut self) -> Vec<(u32, u32)> {
        self.trace.as_mut().map(core::mem::take).unwrap_or_default()
    }

    #[inline]
    fn record(&mut self, bound: u32, result: u32) {
        if let Some(trace) = &mut self.trace {
            trace.push((bound, result));
        }
    }

    /// The next raw 32-bit value — Showdown's `rng.next()`.
    #[inline]
    pub fn next_u32(&mut self) -> u32 {
        let x = self.raw();
        self.record(0, x);
        x
    }

    #[inline]
    fn raw(&mut self) -> u32 {
        match &mut self.backend {
            Backend::Xoshiro(s) => {
                // xoshiro256++ (Blackman & Vigna); upper bits are the strongest.
                let result = s[0].wrapping_add(s[3]).rotate_left(23).wrapping_add(s[0]);
                let t = s[1] << 17;
                s[2] ^= s[0];
                s[3] ^= s[1];
                s[1] ^= s[2];
                s[0] ^= s[3];
                s[2] ^= t;
                s[3] = s[3].rotate_left(45);
                (result >> 32) as u32
            }
            Backend::Gen5(s) => {
                *s = s.wrapping_mul(GEN5_MUL).wrapping_add(GEN5_ADD);
                (*s >> 32) as u32
            }
            Backend::Sodium(key) => sodium_next(key),
        }
    }

    /// Uniform integer in `0..n` — Showdown's `random(n)`:
    /// `floor(next * n / 2^32)`.
    #[inline]
    pub fn random(&mut self, n: u32) -> u32 {
        let x = ((self.raw() as u64 * n as u64) >> 32) as u32;
        self.record(n, x);
        x
    }

    /// Uniform integer in `from..to` — Showdown's `random(from, to)`.
    ///
    /// # Panics
    ///
    /// If `from >= to` (the range is empty).
    #[inline]
    pub fn random_range(&mut self, from: u32, to: u32) -> u32 {
        assert!(from < to, "random_range: empty range {from}..{to}");
        from + self.random(to - from)
    }

    /// `true` with probability `numerator / denominator` — Showdown's
    /// `randomChance`.
    #[inline]
    pub fn chance(&mut self, numerator: u32, denominator: u32) -> bool {
        self.random(denominator) < numerator
    }

    /// Uniformly chosen element — Showdown's `sample`.
    ///
    /// # Panics
    ///
    /// If `items` is empty.
    #[inline]
    pub fn sample<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.random(items.len() as u32) as usize]
    }

    /// Shuffles `items` in place with the draws of Showdown's
    /// `shuffle(items)` (Fisher-Yates from the front, one `random(i, len)`
    /// per step).
    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        let end = items.len() as u32;
        let mut start = 0;
        while start + 1 < end {
            let next = self.random_range(start, end);
            items.swap(start as usize, next as usize);
            start += 1;
        }
    }

    /// The seed of a generator that continues from here (Showdown's
    /// `prng.getSeed()`), for the Showdown backends; `None` for the fast
    /// one, whose state is not a seed.
    pub fn current_seed(&self) -> Option<Seed> {
        match self.backend {
            Backend::Xoshiro(_) => None,
            Backend::Gen5(s) => {
                Some(Seed::ShowdownGen5([(s >> 48) as u16, (s >> 32) as u16, (s >> 16) as u16, s as u16]))
            }
            Backend::Sodium(key) => Some(Seed::Sodium(key)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_stream() {
        for seed in [Seed::Fast(1), Seed::ShowdownGen5([1, 2, 3, 4]), Seed::Sodium([7; 32])] {
            let mut a = BattleRng::new(seed);
            let mut b = BattleRng::new(seed);
            for _ in 0..1000 {
                assert_eq!(a.next_u32(), b.next_u32());
            }
        }
        let mut a = BattleRng::new(Seed::Fast(1));
        let mut b = BattleRng::new(Seed::Fast(2));
        assert!((0..16).any(|_| a.next_u32() != b.next_u32()));
    }

    #[test]
    fn gen5_lcg_step() {
        // One LCG step from seed 0 is just the additive constant.
        let mut r = BattleRng::new(Seed::ShowdownGen5([0, 0, 0, 0]));
        assert_eq!(r.raw(), 0);
        assert_eq!(r.backend, Backend::Gen5(GEN5_ADD));
    }

    fn hex_key(hex: &str) -> [u8; 32] {
        let mut key = [0u8; 32];
        for (i, byte) in key.iter_mut().enumerate() {
            *byte = hex.get(2 * i..2 * i + 2).map_or(0, |h| u8::from_str_radix(h, 16).unwrap());
        }
        key
    }

    /// Showdown's `new PRNG(seed).rng.next()`, six times, and
    /// `getSeed()` after them (Showdown 0.11.11, `ts-chacha20` 1.2.0).
    #[test]
    fn sodium_matches_showdown() {
        let cases: [(&str, [u32; 6], &str); 3] = [
            (
                "00000000000000000000000000000000",
                [4267713560, 2597593732, 1237973749, 687352079, 630900176, 1586261390],
                "06d32c6181e4b678933afaefc102a2da89a01d9c35c6cb37f9a0d8e0a631cce3",
            ),
            (
                "0123456789abcdef0123456789abcdef",
                [1564598223, 2628916691, 3021397871, 1916167142, 4157841405, 1076669892],
                "9190dbf6d492ab7e91153e435ae97e24632ace90e8b61a1bddf9b873b681f336",
            ),
            (
                "ffeeddccbbaa99887766554433221100ffeeddccbbaa99887766554433221100",
                [13129574, 1904100694, 612050774, 578974381, 2540029421, 3029644159],
                "d4a6695bb22b8db31021a2e1e855e5240b20ac3c7a35a1c3b25a4113c2d3d04d",
            ),
        ];
        for (seed, draws, after) in cases {
            let mut r = BattleRng::new(Seed::Sodium(hex_key(seed)));
            let ours: Vec<u32> = (0..6).map(|_| r.next_u32()).collect();
            assert_eq!(ours, draws, "sodium,{seed}");
            assert_eq!(r.backend, Backend::Sodium(hex_key(after)), "sodium,{seed}");
            assert_eq!(r.current_seed(), Some(Seed::Sodium(hex_key(after))));
        }
    }

    #[test]
    fn current_seed_continues_the_stream() {
        for seed in [Seed::ShowdownGen5([1, 2, 3, 4]), Seed::Sodium([9; 32])] {
            let mut a = BattleRng::new(seed);
            for _ in 0..5 {
                a.next_u32();
            }
            let mut b = BattleRng::new(a.current_seed().unwrap());
            assert!((0..10).all(|_| a.next_u32() == b.next_u32()));
        }
        assert_eq!(BattleRng::new(Seed::Fast(1)).current_seed(), None);
    }

    #[test]
    fn chacha20_rfc8439_block() {
        // RFC 8439, 2.3.2.
        let key: [u8; 32] = core::array::from_fn(|i| i as u8);
        let nonce = [0, 0, 0, 9, 0, 0, 0, 0x4a, 0, 0, 0, 0];
        let block = chacha20_block(&key, 1, &nonce);
        assert_eq!(
            &block[..16],
            &[0x10, 0xf1, 0xe7, 0xe4, 0xd1, 0x3b, 0x59, 0x15, 0x50, 0x0f, 0xdd, 0x1f, 0xa3, 0x20, 0x71, 0xc4]
        );
    }

    #[test]
    fn trace_records_draws_without_changing_them() {
        let mut plain = BattleRng::new(Seed::Fast(5));
        let mut traced = plain.clone();
        traced.enable_trace();
        let a: Vec<u32> = (1..10).map(|n| plain.random(n)).collect();
        let b: Vec<u32> = (1..10).map(|n| traced.random(n)).collect();
        assert_eq!(a, b);
        assert_eq!(plain, traced);
        let trace = traced.take_trace();
        assert_eq!(trace.len(), 9);
        assert_eq!(trace[3], (4, b[3]));
        assert!(traced.take_trace().is_empty());
        assert!(plain.take_trace().is_empty());
    }

    #[test]
    fn ranges_and_shuffle() {
        let mut r = BattleRng::new(Seed::Fast(99));
        let mut counts = [0u32; 16];
        for _ in 0..16_000 {
            let roll = r.random_range(85, 101);
            assert!((85..101).contains(&roll));
            counts[(roll - 85) as usize] += 1;
        }
        assert!(counts.iter().all(|&c| c > 800), "{counts:?}");

        let mut v = [0, 1, 2, 3, 4, 5];
        r.shuffle(&mut v);
        let mut sorted = v;
        sorted.sort_unstable();
        assert_eq!(sorted, [0, 1, 2, 3, 4, 5]);
    }
}
