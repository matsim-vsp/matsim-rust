use fnv::FnvHasher;
use rand::SeedableRng;
use rand_xoshiro::Xoshiro256PlusPlus;
use std::hash::Hasher;

// Bump this version deliberately whenever the seed encoding changes. It prevents a new encoding
// from silently producing streams that look compatible with the old reproducibility contract.
const RNG_SEED_FORMAT: &[u8] = b"parallel_qsim.rng.v1";

/// Creates a fast, portable RNG stream from explicitly encoded seed material.
///
/// `Xoshiro256PlusPlus` is intentionally used instead of an implementation-defined convenience
/// generator such as `SmallRng`: it is a named, fast, non-cryptographic algorithm whose fixed-width
/// integer operations produce the same stream on 32- and 64-bit platforms.
pub fn get_rng(base_seed: u64, purpose: &str, stream_id: &str) -> Xoshiro256PlusPlus {
    Xoshiro256PlusPlus::seed_from_u64(derive_seed(base_seed, purpose, stream_id))
}

fn derive_seed(base_seed: u64, purpose: &str, stream_id: &str) -> u64 {
    // FNV-1a-64 is deliberately used as a small and fast deterministic mixer, not for security.
    // Unlike a randomized/default hasher, its algorithm and initial state are fixed. We feed it
    // explicit bytes instead of Rust's generic `Hash` encoding, which is not a portable format and
    // is therefore unsuitable for a reproducibility contract.
    let mut hasher = FnvHasher::default();
    hasher.write(RNG_SEED_FORMAT);

    // Native endianness differs between platforms. Little endian makes the representation of every
    // numeric seed field explicit and identical on all supported architectures.
    hasher.write(&base_seed.to_le_bytes());

    // Length prefixes preserve field boundaries: ("ab", "c") must not hash like ("a", "bc").
    write_len_prefixed(&mut hasher, purpose.as_bytes());
    write_len_prefixed(&mut hasher, stream_id.as_bytes());

    hasher.finish()
}

fn write_len_prefixed(hasher: &mut FnvHasher, bytes: &[u8]) {
    // `usize` is platform-sized, so normalize the length to u64 and encode it in little endian.
    let len = bytes.len();
    hasher.write(
        &u64::try_from(len)
            .expect("seed component length does not fit into u64")
            .to_le_bytes(),
    );
    hasher.write(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::RngExt;

    #[test]
    fn fnv1a64_matches_reference_vectors() {
        for (input, expected) in [
            (&b""[..], 0xcbf2_9ce4_8422_2325),
            (&b"a"[..], 0xaf63_dc4c_8601_ec8c),
            (&b"foobar"[..], 0x8594_4171_f739_67e8),
        ] {
            let mut hasher = FnvHasher::default();
            hasher.write(input);
            assert_eq!(expected, hasher.finish());
        }
    }

    #[test]
    fn stable_seed_encoding_has_no_field_ambiguity() {
        let split_after_two = derive_seed(42, "ab", "c");
        let split_after_one = derive_seed(42, "a", "bc");

        assert_ne!(split_after_two, split_after_one);
    }

    #[test]
    fn seed_inputs_define_independent_streams() {
        let seed = derive_seed(42, "replanning.strategy", "7:agent-1");
        let mut first = get_rng(42, "replanning.strategy", "7:agent-1");
        let mut second = get_rng(42, "replanning.strategy", "7:agent-1");

        for _ in 0..4 {
            assert_eq!(first.random::<u64>(), second.random::<u64>());
        }

        assert_ne!(seed, derive_seed(43, "replanning.strategy", "7:agent-1"));
        assert_ne!(seed, derive_seed(42, "replanning.selector", "7:agent-1"));
        assert_ne!(seed, derive_seed(42, "replanning.strategy", "8:agent-1"));
    }

    #[test]
    fn rng_stream_matches_golden_vector() {
        let seed = derive_seed(42, "replanning.strategy", "7:agent-1");
        let mut rng = get_rng(42, "replanning.strategy", "7:agent-1");

        assert_eq!(0xc20c_4b2e_45d1_8cfa, seed);
        assert_eq!(
            [
                0xd496_7079_c7e2_c9b0,
                0x1b53_f672_809d_8b63,
                0x0047_6f1a_aaa3_2af3,
                0x0b38_ffc3_7f7a_8faf,
            ],
            std::array::from_fn(|_| rng.random::<u64>())
        );
    }
}
