// XXH64 streaming algorithm used by Zstandard frame checksums, seed zero.
use core::{convert::TryInto, hash::Hasher};
const P1: u64 = 11400714785074694791;
const P2: u64 = 14029467366897019727;
const P3: u64 = 1609587929392839161;
const P4: u64 = 9650029242287828579;
const P5: u64 = 2870177450012600261;
#[derive(Clone)]
pub struct XxHash64 {
    total: u64,
    lanes: [u64; 4],
    tail: [u8; 32],
    used: usize,
    seed: u64,
}
fn round(acc: u64, value: u64) -> u64 {
    acc.wrapping_add(value.wrapping_mul(P2))
        .rotate_left(31)
        .wrapping_mul(P1)
}
fn merge(acc: u64, value: u64) -> u64 {
    (acc ^ round(0, value)).wrapping_mul(P1).wrapping_add(P4)
}
fn word(bytes: &[u8]) -> u64 {
    u64::from_le_bytes(bytes[..8].try_into().unwrap())
}
impl XxHash64 {
    pub fn with_seed(seed: u64) -> Self {
        Self {
            total: 0,
            lanes: [
                seed.wrapping_add(P1).wrapping_add(P2),
                seed.wrapping_add(P2),
                seed,
                seed.wrapping_sub(P1),
            ],
            tail: [0; 32],
            used: 0,
            seed,
        }
    }
    fn stripe(&mut self, bytes: &[u8]) {
        for (index, lane) in self.lanes.iter_mut().enumerate() {
            *lane = round(*lane, word(&bytes[index * 8..]));
        }
    }
}
impl Hasher for XxHash64 {
    fn write(&mut self, mut bytes: &[u8]) {
        self.total = self.total.wrapping_add(bytes.len() as u64);
        if bytes.len() < 32 - self.used {
            self.tail[self.used..self.used + bytes.len()].copy_from_slice(bytes);
            self.used += bytes.len();
            return;
        }
        if self.used != 0 {
            let count = 32 - self.used;
            self.tail[self.used..].copy_from_slice(&bytes[..count]);
            let stripe = self.tail;
            self.stripe(&stripe);
            bytes = &bytes[count..];
            self.used = 0;
        }
        while bytes.len() >= 32 {
            self.stripe(&bytes[..32]);
            bytes = &bytes[32..];
        }
        self.tail[..bytes.len()].copy_from_slice(bytes);
        self.used = bytes.len();
    }
    fn finish(&self) -> u64 {
        let mut hash = if self.total >= 32 {
            let mut h = self.lanes[0]
                .rotate_left(1)
                .wrapping_add(self.lanes[1].rotate_left(7))
                .wrapping_add(self.lanes[2].rotate_left(12))
                .wrapping_add(self.lanes[3].rotate_left(18));
            for lane in self.lanes {
                h = merge(h, lane);
            }
            h
        } else {
            self.seed.wrapping_add(P5)
        };
        hash = hash.wrapping_add(self.total);
        let mut bytes = &self.tail[..self.used];
        while bytes.len() >= 8 {
            hash ^= round(0, word(bytes));
            hash = hash.rotate_left(27).wrapping_mul(P1).wrapping_add(P4);
            bytes = &bytes[8..];
        }
        if bytes.len() >= 4 {
            hash ^= (u32::from_le_bytes(bytes[..4].try_into().unwrap()) as u64).wrapping_mul(P1);
            hash = hash.rotate_left(23).wrapping_mul(P2).wrapping_add(P3);
            bytes = &bytes[4..];
        }
        for byte in bytes {
            hash ^= (*byte as u64).wrapping_mul(P5);
            hash = hash.rotate_left(11).wrapping_mul(P1);
        }
        hash ^= hash >> 33;
        hash = hash.wrapping_mul(P2);
        hash ^= hash >> 29;
        hash = hash.wrapping_mul(P3);
        hash ^= hash >> 32;
        hash
    }
}

#[cfg(any())]
mod tests {
    use super::XxHash64;
    use core::hash::Hasher;

    #[cfg(any())]
    #[test]
    fn agrees_with_reference_across_streaming_boundaries() {
        let bytes: alloc::vec::Vec<_> = (0..4097).map(|i| (i * 37 + i / 251) as u8).collect();
        for seed in [0, 1, u64::MAX] {
            for len in [0, 1, 3, 4, 7, 8, 15, 31, 32, 33, 63, 64, 65, 4097] {
                let mut reference = twox_hash::XxHash64::with_seed(seed);
                reference.write(&bytes[..len]);
                for chunk in [1, 7, 31, 32, 33, 64, 4097] {
                    let mut actual = XxHash64::with_seed(seed);
                    for part in bytes[..len].chunks(chunk) {
                        actual.write(part);
                        actual.write(&[]);
                    }
                    assert_eq!(
                        actual.finish(),
                        reference.finish(),
                        "seed={seed}, len={len}, chunk={chunk}"
                    );
                }
            }
        }
    }
}
