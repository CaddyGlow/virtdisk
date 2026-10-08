use crate::zstd_decoder::io::{Error, Read};
use alloc::{collections::VecDeque, vec};
pub struct RingBuffer {
    bytes: VecDeque<u8>,
}
impl RingBuffer {
    pub fn new() -> Self {
        Self {
            bytes: VecDeque::new(),
        }
    }
    pub fn len(&self) -> usize {
        self.bytes.len()
    }
    pub fn clear(&mut self) {
        self.bytes.clear()
    }
    pub fn reserve(&mut self, amount: usize) {
        self.bytes.reserve(amount)
    }
    pub fn extend(&mut self, data: &[u8]) {
        self.bytes.extend(data.iter().copied())
    }
    pub fn drop_first_n(&mut self, amount: usize) {
        self.bytes.drain(..amount);
    }
    pub fn as_slices(&self) -> (&[u8], &[u8]) {
        self.bytes.as_slices()
    }
    pub fn extend_from_within(&mut self, start: usize, len: usize) {
        let end = start.checked_add(len).expect("checked match range");
        assert!(end <= self.bytes.len());
        self.bytes.reserve(len);
        for index in start..end {
            let byte = self.bytes[index];
            self.bytes.push_back(byte);
        }
    }
    pub fn extend_and_fill(&mut self, byte: u8, len: usize) {
        self.bytes.resize(self.bytes.len() + len, byte)
    }
    pub fn extend_from_reader<R: Read>(&mut self, mut read: R, len: usize) -> Result<(), Error> {
        let mut bytes = vec![0; len];
        read.read_exact(&mut bytes)?;
        self.bytes.extend(bytes);
        Ok(())
    }
}

#[cfg(any())]
mod tests {
    use super::RingBuffer;
    use alloc::vec::Vec;

    #[cfg(any())]
    #[test]
    fn ringbuffer_matches_linear_storage_after_wrap_and_growth() {
        let mut ring = RingBuffer::new();
        let mut expected = Vec::new();
        ring.reserve(32);
        for step in 0..1000 {
            let data = [step as u8; 17];
            ring.extend(&data);
            expected.extend_from_slice(&data);
            let start = expected.len() / 3;
            let len = (expected.len() - start).min(13);
            ring.extend_from_within(start, len);
            expected.extend_from_within(start..start + len);
            ring.extend_and_fill(42, 5);
            expected.extend_from_slice(&[42; 5]);
            let drop = expected.len().saturating_sub(51);
            ring.drop_first_n(drop);
            expected.drain(..drop);
            let (first, second) = ring.as_slices();
            assert_eq!([first, second].concat(), expected);
            assert_eq!(ring.len(), expected.len());
        }
        ring.clear();
        ring.extend_from_reader(&b"after-reset"[..], 11).unwrap();
        let (first, second) = ring.as_slices();
        assert_eq!([first, second].concat(), b"after-reset");
    }
}
