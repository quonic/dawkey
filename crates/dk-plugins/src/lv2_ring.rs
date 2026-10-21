// SPDX-License-Identifier: AGPL-3.0-or-later
//! Messages of bytes through an `rtrb` ring, between a plugin's audio
//! thread and another: worker requests and responses, and atoms between an
//! LV2 plugin and its UI. Each message is a tag (a port, say), a length and
//! that many bytes. Writing and reading never allocate, so either end may
//! be the real-time one.

/// The tag and the length in front of each message.
const HEADER: usize = 8;

/// A ring of messages.
pub(crate) fn ring(capacity: usize) -> (MessageWriter, MessageReader) {
    let (producer, consumer) = rtrb::RingBuffer::new(capacity);
    (MessageWriter(producer), MessageReader(consumer))
}

/// The writing end of a [`ring`].
pub(crate) struct MessageWriter(rtrb::Producer<u8>);

impl std::fmt::Debug for MessageWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MessageWriter")
    }
}

impl MessageWriter {
    /// Writes one message of `parts`, one after the other, tagged `tag`.
    /// Returns false (writing nothing) if it doesn't fit.
    pub(crate) fn write(&mut self, tag: u32, parts: &[&[u8]]) -> bool {
        let len: usize = parts.iter().map(|p| p.len()).sum();
        let Ok(len32) = u32::try_from(len) else {
            return false;
        };
        let Ok(mut chunk) = self.0.write_chunk_uninit(HEADER + len) else {
            return false;
        };
        let (a, b) = chunk.as_mut_slices();
        let mut header = [0u8; HEADER];
        header[..4].copy_from_slice(&tag.to_ne_bytes());
        header[4..].copy_from_slice(&len32.to_ne_bytes());
        let mut at = 0;
        for part in std::iter::once(&header[..]).chain(parts.iter().copied()) {
            copy_in(a, b, at, part);
            at += part.len();
        }
        // SAFETY: every slot of the chunk was written above.
        unsafe { chunk.commit_all() };
        true
    }
}

/// Copies `bytes` to position `at` of `a` followed by `b`.
fn copy_in(
    a: &mut [std::mem::MaybeUninit<u8>],
    b: &mut [std::mem::MaybeUninit<u8>],
    at: usize,
    bytes: &[u8],
) {
    let copy = |dst: &mut [std::mem::MaybeUninit<u8>], src: &[u8]| {
        // SAFETY: `dst` is as long as `src`; `MaybeUninit<u8>` is laid out
        // as `u8`.
        unsafe { std::ptr::copy_nonoverlapping(src.as_ptr(), dst.as_mut_ptr().cast(), src.len()) };
    };
    let first = a.len().saturating_sub(at).min(bytes.len());
    let start = at.min(a.len());
    copy(&mut a[start..start + first], &bytes[..first]);
    let rest = &bytes[first..];
    if !rest.is_empty() {
        // What's left starts in `b` (`a` is full up to here).
        let from = at + first - a.len();
        copy(&mut b[from..from + rest.len()], rest);
    }
}

/// The reading end of a [`ring`].
pub(crate) struct MessageReader(rtrb::Consumer<u8>);

impl std::fmt::Debug for MessageReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MessageReader")
    }
}

impl MessageReader {
    /// Reads every message written so far: `f` gets each one's tag and its
    /// bytes, in two parts (where the ring wraps; the second is often
    /// empty).
    pub(crate) fn read(&mut self, mut f: impl FnMut(u32, [&[u8]; 2])) {
        let available = self.0.slots();
        let Ok(chunk) = self.0.read_chunk(available) else {
            return;
        };
        let (a, b) = chunk.as_slices();
        let mut at = 0;
        // Messages are written whole, so what's there is whole messages.
        while at + HEADER <= available {
            let mut header = [0u8; HEADER];
            for (i, byte) in header.iter_mut().enumerate() {
                *byte = byte_at(a, b, at + i);
            }
            let tag = u32::from_ne_bytes(header[..4].try_into().expect("4 bytes"));
            let len = u32::from_ne_bytes(header[4..].try_into().expect("4 bytes")) as usize;
            let start = at + HEADER;
            if start + len > available {
                break;
            }
            f(tag, split(a, b, start, len));
            at = start + len;
        }
        chunk.commit(at);
    }
}

fn byte_at(a: &[u8], b: &[u8], i: usize) -> u8 {
    if i < a.len() { a[i] } else { b[i - a.len()] }
}

/// Bytes `start..start + len` of `a` followed by `b`, as two slices.
fn split<'a>(a: &'a [u8], b: &'a [u8], start: usize, len: usize) -> [&'a [u8]; 2] {
    let end = start + len;
    if end <= a.len() {
        [&a[start..end], &[]]
    } else if start >= a.len() {
        [&b[start - a.len()..end - a.len()], &[]]
    } else {
        [&a[start..], &b[..end - a.len()]]
    }
}

/// Copies `parts` into `words` (8-byte aligned, for plugins that read what
/// they're given as structs), growing it only if it's too small. Returns
/// the bytes.
pub(crate) fn gather<'a>(words: &'a mut Vec<u64>, parts: [&[u8]; 2]) -> &'a [u8] {
    let len = parts[0].len() + parts[1].len();
    let needed = len.div_ceil(8);
    if words.len() < needed {
        words.resize(needed, 0);
    }
    // SAFETY: `[u64]` is valid as bytes, and it holds at least `len`.
    let bytes = unsafe { std::slice::from_raw_parts_mut(words.as_mut_ptr().cast::<u8>(), len) };
    bytes[..parts[0].len()].copy_from_slice(parts[0]);
    bytes[parts[0].len()..].copy_from_slice(parts[1]);
    bytes
}

/// The first `len` bytes of `words`.
pub(crate) fn bytes(words: &[u64], len: usize) -> &[u8] {
    // SAFETY: `[u64]` is valid as bytes; `len` is clamped to its size.
    unsafe { std::slice::from_raw_parts(words.as_ptr().cast::<u8>(), len.min(words.len() * 8)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_come_out_whole_and_in_order_across_the_wrap() {
        let (mut writer, mut reader) = ring(64);
        let mut seen = Vec::new();
        for round in 0..20u8 {
            let body = vec![round; usize::from(round % 7) * 3];
            assert!(writer.write(
                u32::from(round),
                &[&body[..1.min(body.len())], &body[1.min(body.len())..]]
            ));
            reader.read(|tag, [a, b]| seen.push((tag, [a, b].concat())));
        }
        let expected: Vec<(u32, Vec<u8>)> = (0..20u8)
            .map(|r| (u32::from(r), vec![r; usize::from(r % 7) * 3]))
            .collect();
        assert_eq!(seen, expected);
        // What doesn't fit isn't written at all.
        assert!(!writer.write(1, &[&[0; 60]]));
        let mut none = true;
        reader.read(|_, _| none = false);
        assert!(none);
    }

    #[test]
    fn gathered_bytes_are_aligned() {
        let mut words = Vec::new();
        let bytes = gather(&mut words, [&[1, 2, 3], &[4, 5]]);
        assert_eq!(bytes, [1, 2, 3, 4, 5]);
        assert_eq!(bytes.as_ptr() as usize % 8, 0);
    }
}
