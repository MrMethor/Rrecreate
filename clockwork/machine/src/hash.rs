//! A small stable hash for state and frame fingerprints. Not cryptographic: it only needs to be
//! identical across runs, platforms and Rust versions, which `std`'s default hasher isn't.

use std::hash::Hasher;

const OFFSET_BASIS: u64 = 0xCBF2_9CE4_8422_2325;
const PRIME: u64 = 0x0000_0100_0000_01B3;

/// 64-bit FNV-1a.
#[derive(Clone, Copy, Debug)]
pub struct Fnv64(u64);

impl Default for Fnv64 {
    fn default() -> Self {
        Self(OFFSET_BASIS)
    }
}

impl Hasher for Fnv64 {
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ u64::from(b)).wrapping_mul(PRIME);
        }
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

pub fn fnv64(bytes: &[u8]) -> u64 {
    let mut h = Fnv64::default();
    h.write(bytes);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors() {
        assert_eq!(fnv64(b""), 0xCBF2_9CE4_8422_2325);
        assert_eq!(fnv64(b"a"), 0xAF63_DC4C_8601_EC8C);
        assert_eq!(fnv64(b"foobar"), 0x85944171F73967E8);
    }
}
