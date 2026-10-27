//! Minimal PNG encoder: 8-bit RGB, adaptive row filters, zlib stream with
//! LZ77 and fixed Huffman codes.

use alloc::vec;
use alloc::vec::Vec;

/// Encodes `0x00RRGGBB` pixels as a PNG file.
pub fn encode(width: usize, height: usize, pixels: &[u32]) -> Vec<u8> {
    assert_eq!(pixels.len(), width * height);
    assert!(width > 0 && height > 0 && width <= 0x7fff_ffff && height <= 0x7fff_ffff);
    let raw = filter_rows(width, height, pixels);
    let mut out = Vec::with_capacity(raw.len() / 2 + 64);
    out.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&(width as u32).to_be_bytes());
    ihdr.extend_from_slice(&(height as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit RGB, deflate, adaptive, no interlace
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &zlib(&raw));
    chunk(&mut out, b"IEND", &[]);
    out
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (crc & 1).wrapping_neg());
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for block in data.chunks(5552) {
        for &x in block {
            a += x as u32;
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    (b << 16) | a
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = a as i16 + b as i16 - c as i16;
    let (pa, pb, pc) = ((p - a as i16).abs(), (p - b as i16).abs(), (p - c as i16).abs());
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

/// Picks, per row, the filter with the smallest sum of absolute values.
fn filter_rows(width: usize, height: usize, pixels: &[u32]) -> Vec<u8> {
    let stride = width * 3;
    let mut out = Vec::with_capacity((stride + 1) * height);
    let mut prev = vec![0u8; stride];
    let mut cur = vec![0u8; stride];
    let mut cand = vec![0u8; stride];
    let mut best = vec![0u8; stride];
    for y in 0..height {
        for (x, &p) in pixels[y * width..(y + 1) * width].iter().enumerate() {
            cur[3 * x] = (p >> 16) as u8;
            cur[3 * x + 1] = (p >> 8) as u8;
            cur[3 * x + 2] = p as u8;
        }
        let mut best_score = u64::MAX;
        let mut best_kind = 0;
        for kind in 0..5u8 {
            for i in 0..stride {
                let a = if i >= 3 { cur[i - 3] } else { 0 };
                let b = prev[i];
                let c = if i >= 3 { prev[i - 3] } else { 0 };
                let pred = match kind {
                    0 => 0,
                    1 => a,
                    2 => b,
                    3 => ((a as u16 + b as u16) / 2) as u8,
                    _ => paeth(a, b, c),
                };
                cand[i] = cur[i].wrapping_sub(pred);
            }
            let score: u64 = cand.iter().map(|&v| (v as i8).unsigned_abs() as u64).sum();
            if score < best_score {
                best_score = score;
                best_kind = kind;
                best.copy_from_slice(&cand);
            }
        }
        out.push(best_kind);
        out.extend_from_slice(&best);
        core::mem::swap(&mut prev, &mut cur);
    }
    out
}

struct Bits {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl Bits {
    fn put(&mut self, value: u32, count: u32) {
        self.acc |= (value as u64) << self.n;
        self.n += count;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }

    /// Huffman codes are sent most significant bit first.
    fn code(&mut self, code: u32, len: u32) {
        self.put(code.reverse_bits() >> (32 - len), len);
    }

    fn literal(&mut self, sym: u32) {
        match sym {
            0..=143 => self.code(0x30 + sym, 8),
            144..=255 => self.code(0x190 + sym - 144, 9),
            256..=279 => self.code(sym - 256, 7),
            _ => self.code(0xc0 + sym - 280, 8),
        }
    }
}

const LEN_BASE: [u16; 29] =
    [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
const LEN_EXTRA: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145,
    8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] =
    [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];

const WINDOW: usize = 32768;
const HASH_BITS: u32 = 15;
const MAX_CHAIN: usize = 48;

fn hash(d: &[u8], i: usize) -> usize {
    let v = (d[i] as u32) << 16 | (d[i + 1] as u32) << 8 | d[i + 2] as u32;
    (v.wrapping_mul(0x9E37_79B1) >> (32 - HASH_BITS)) as usize
}

fn zlib(data: &[u8]) -> Vec<u8> {
    let mut bits = Bits { out: vec![0x78, 0x9c], acc: 0, n: 0 };
    bits.put(1, 1); // final block
    bits.put(1, 2); // fixed Huffman codes
    let mut head = vec![usize::MAX; 1 << HASH_BITS];
    let mut prev = vec![usize::MAX; WINDOW];
    let insert = |head: &mut Vec<usize>, prev: &mut Vec<usize>, i: usize| {
        if i + 2 < data.len() {
            let h = hash(data, i);
            prev[i % WINDOW] = head[h];
            head[h] = i;
        }
    };
    let mut i = 0;
    while i < data.len() {
        let (mut best_len, mut best_dist) = (0, 0);
        if i + 2 < data.len() {
            let mut cand = head[hash(data, i)];
            let max = (data.len() - i).min(258);
            let mut chain = 0;
            while cand != usize::MAX && i - cand <= WINDOW && chain < MAX_CHAIN {
                if data[cand + best_len.min(max - 1)] == data[i + best_len.min(max - 1)] {
                    let l = data[cand..].iter().zip(&data[i..i + max]).take_while(|(a, b)| a == b).count();
                    if l > best_len {
                        best_len = l;
                        best_dist = i - cand;
                        if l == max {
                            break;
                        }
                    }
                }
                let next = prev[cand % WINDOW];
                if next == usize::MAX || next >= cand {
                    break;
                }
                cand = next;
                chain += 1;
            }
        }
        if best_len >= 3 {
            let li = LEN_BASE.iter().rposition(|&b| b as usize <= best_len).unwrap();
            bits.literal(257 + li as u32);
            bits.put((best_len - LEN_BASE[li] as usize) as u32, LEN_EXTRA[li] as u32);
            let di = DIST_BASE.iter().rposition(|&b| b as usize <= best_dist).unwrap();
            bits.code(di as u32, 5);
            bits.put((best_dist - DIST_BASE[di] as usize) as u32, DIST_EXTRA[di] as u32);
            for k in i..i + best_len {
                insert(&mut head, &mut prev, k);
            }
            i += best_len;
        } else {
            bits.literal(data[i] as u32);
            insert(&mut head, &mut prev, i);
            i += 1;
        }
    }
    bits.literal(256);
    if bits.n > 0 {
        bits.put(0, 8 - bits.n);
    }
    let mut out = bits.out;
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksums() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
        assert_eq!(adler32(b"Wikipedia"), 0x11e6_0398);
    }

    #[test]
    fn length_table_is_consistent() {
        // Every length 3..=258 must encode to a base plus in-range extra bits.
        for len in 3..=258usize {
            let li = LEN_BASE.iter().rposition(|&b| b as usize <= len).unwrap();
            assert!(len - (LEN_BASE[li] as usize) < 1 << LEN_EXTRA[li], "{len}");
        }
        for d in 1..=32768usize {
            let di = DIST_BASE.iter().rposition(|&b| b as usize <= d).unwrap();
            assert!(d - (DIST_BASE[di] as usize) < 1 << DIST_EXTRA[di], "{d}");
        }
    }
}
