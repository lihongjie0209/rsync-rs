#![allow(dead_code)]

use crate::checksum::md4::{md4_classic, md4_modern};
use md5::Md5;
use digest::Digest;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChecksumType {
    Md4Archaic,
    Md4Busted,
    Md4Old,
    Md4,
    Md5,
    None,
}

impl ChecksumType {
    pub fn for_protocol(version: u32, _proper_seed_order: bool) -> Self {
        if version >= 30 {
            ChecksumType::Md5
        } else if version >= 27 {
            ChecksumType::Md4Old
        } else if version >= 21 {
            ChecksumType::Md4Busted
        } else {
            ChecksumType::Md4Archaic
        }
    }

    pub fn digest_len(&self) -> usize {
        match self {
            ChecksumType::None => 0,
            _ => 16,
        }
    }
}

pub struct StrongChecksum {
    pub ctype: ChecksumType,
}

impl StrongChecksum {
    pub fn new(ctype: ChecksumType) -> Self {
        Self { ctype }
    }

    pub fn compute(
        data: &[u8],
        ctype: ChecksumType,
        seed: u32,
        proper_seed_order: bool,
    ) -> Vec<u8> {
        match ctype {
            ChecksumType::Md5 => {
                let mut hasher = Md5::new();
                if proper_seed_order {
                    if seed != 0 { hasher.update(seed.to_le_bytes()); }
                    hasher.update(data);
                } else {
                    hasher.update(data);
                    if seed != 0 { hasher.update(seed.to_le_bytes()); }
                }
                hasher.finalize().to_vec()
            }
            ChecksumType::Md4Old | ChecksumType::Md4 => {
                let mut buf = data.to_vec();
                if seed != 0 { buf.extend_from_slice(&seed.to_le_bytes()); }
                md4_modern(&buf).to_vec()
            }
            ChecksumType::Md4Busted | ChecksumType::Md4Archaic => {
                let mut buf = data.to_vec();
                if seed != 0 { buf.extend_from_slice(&seed.to_le_bytes()); }
                md4_classic(&buf).to_vec()
            }
            ChecksumType::None => vec![0],
        }
    }

    pub fn file_checksum(data: &[u8], ctype: ChecksumType) -> Vec<u8> {
        Self::compute(data, ctype, 0, false)
    }
}

pub struct SumHead {
    pub blength: i32,
    pub s2length: i32,
    pub count: i64,
    pub remainder: i32,
}

impl SumHead {
    pub fn read<R: std::io::Read>(r: &mut R, protocol_version: u32) -> anyhow::Result<Self> {
        let mut buf = [0u8; 4];

        r.read_exact(&mut buf)?;
        let count = i32::from_le_bytes(buf) as i64;

        r.read_exact(&mut buf)?;
        let blength = i32::from_le_bytes(buf);

        let s2length = if protocol_version >= 27 {
            r.read_exact(&mut buf)?;
            i32::from_le_bytes(buf)
        } else {
            0
        };

        r.read_exact(&mut buf)?;
        let remainder = i32::from_le_bytes(buf);

        Ok(SumHead { blength, s2length, count, remainder })
    }

    pub fn write<W: std::io::Write>(
        &self,
        w: &mut W,
        protocol_version: u32,
    ) -> anyhow::Result<()> {
        w.write_all(&(self.count as i32).to_le_bytes())?;
        w.write_all(&self.blength.to_le_bytes())?;
        if protocol_version >= 27 {
            w.write_all(&self.s2length.to_le_bytes())?;
        }
        w.write_all(&self.remainder.to_le_bytes())?;
        Ok(())
    }

    pub fn for_file(file_size: i64, csum_length: usize, _protocol_version: u32) -> Self {
        const BLOCK_SIZE: i32 = 700;
        let blength = BLOCK_SIZE;
        let count = if file_size == 0 {
            0i64
        } else {
            (file_size + blength as i64 - 1) / blength as i64
        };
        let remainder = (file_size % blength as i64) as i32;
        SumHead { blength, s2length: csum_length as i32, count, remainder }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn for_protocol_selects_correct_type() {
        assert_eq!(ChecksumType::for_protocol(20, false), ChecksumType::Md4Archaic);
        assert_eq!(ChecksumType::for_protocol(21, false), ChecksumType::Md4Busted);
        assert_eq!(ChecksumType::for_protocol(26, false), ChecksumType::Md4Busted);
        assert_eq!(ChecksumType::for_protocol(27, false), ChecksumType::Md4Old);
        assert_eq!(ChecksumType::for_protocol(29, false), ChecksumType::Md4Old);
        assert_eq!(ChecksumType::for_protocol(30, false), ChecksumType::Md5);
        assert_eq!(ChecksumType::for_protocol(31, true),  ChecksumType::Md5);
    }

    #[test]
    fn digest_len_values() {
        assert_eq!(ChecksumType::Md4Archaic.digest_len(), 16);
        assert_eq!(ChecksumType::Md4Busted.digest_len(),  16);
        assert_eq!(ChecksumType::Md4Old.digest_len(),     16);
        assert_eq!(ChecksumType::Md4.digest_len(),        16);
        assert_eq!(ChecksumType::Md5.digest_len(),        16);
        assert_eq!(ChecksumType::None.digest_len(),        0);
    }

    #[test]
    fn md5_no_seed() {
        let got = StrongChecksum::compute(b"", ChecksumType::Md5, 0, false);
        let expected = [
            0xd4u8, 0x1d, 0x8c, 0xd9, 0x8f, 0x00, 0xb2, 0x04,
            0xe9, 0x80, 0x09, 0x98, 0xec, 0xf8, 0x42, 0x7e,
        ];
        assert_eq!(got.as_slice(), &expected);
    }

    #[test]
    fn md5_hello_world_no_seed() {
        let got = StrongChecksum::compute(b"hello world", ChecksumType::Md5, 0, false);
        let expected = [
            0x5eu8, 0xb6, 0x3b, 0xbb, 0xe0, 0x1e, 0xee, 0xd0,
            0x93, 0xcb, 0x22, 0xbb, 0x8f, 0x5a, 0xcd, 0xc3,
        ];
        assert_eq!(got.as_slice(), &expected);
    }

    #[test]
    fn md5_with_seed_old_order() {
        let seed: u32 = 1;
        let got = StrongChecksum::compute(b"hello world", ChecksumType::Md5, seed, false);
        let expected: Vec<u8> = {
            let mut h = Md5::new();
            h.update(b"hello world");
            h.update(&1u32.to_le_bytes());
            h.finalize().to_vec()
        };
        assert_eq!(got, expected);
    }

    #[test]
    fn md5_with_seed_new_order() {
        let seed: u32 = 1;
        let got = StrongChecksum::compute(b"hello world", ChecksumType::Md5, seed, true);
        let expected: Vec<u8> = {
            let mut h = Md5::new();
            h.update(&1u32.to_le_bytes());
            h.update(b"hello world");
            h.finalize().to_vec()
        };
        assert_eq!(got, expected);
    }

    #[test]
    fn md5_seed_order_matters() {
        let seed: u32 = 42;
        let a = StrongChecksum::compute(b"test data", ChecksumType::Md5, seed, false);
        let b = StrongChecksum::compute(b"test data", ChecksumType::Md5, seed, true);
        assert_ne!(a, b);
    }

    #[test]
    fn md5_zero_seed_order_irrelevant() {
        let a = StrongChecksum::compute(b"data", ChecksumType::Md5, 0, false);
        let b = StrongChecksum::compute(b"data", ChecksumType::Md5, 0, true);
        assert_eq!(a, b);
    }

    #[test]
    fn md4_old_no_seed_matches_rfc() {
        let got = StrongChecksum::compute(b"abc", ChecksumType::Md4Old, 0, false);
        let expected = [
            0xa4u8, 0x48, 0x01, 0x7a, 0xaf, 0x21, 0xd8, 0x52,
            0x5f, 0xc1, 0x0a, 0xe8, 0x7a, 0xa6, 0x72, 0x9d,
        ];
        assert_eq!(got.as_slice(), &expected);
    }

    #[test]
    fn md4_seed_appended_not_prepended() {
        let data = b"test";
        let seed: u32 = 7;
        let a = StrongChecksum::compute(data, ChecksumType::Md4Old, seed, false);
        let b = StrongChecksum::compute(data, ChecksumType::Md4Old, seed, true);
        assert_eq!(a, b);
        let mut buf = data.to_vec();
        buf.extend_from_slice(&seed.to_le_bytes());
        assert_eq!(a, md4_modern(&buf).to_vec());
    }

    #[test]
    fn file_checksum_no_seed() {
        let data = b"hello world";
        assert_eq!(
            StrongChecksum::file_checksum(data, ChecksumType::Md5),
            StrongChecksum::compute(data, ChecksumType::Md5, 0, false),
        );
    }

    #[test]
    fn sum_head_roundtrip_protocol_27() {
        let head = SumHead { blength: 700, s2length: 16, count: 42, remainder: 123 };
        let mut buf = Vec::new();
        head.write(&mut buf, 27).unwrap();
        assert_eq!(buf.len(), 16);
        let got = SumHead::read(&mut buf.as_slice(), 27).unwrap();
        assert_eq!(got.count,     42);
        assert_eq!(got.blength,  700);
        assert_eq!(got.s2length,  16);
        assert_eq!(got.remainder, 123);
    }

    #[test]
    fn sum_head_roundtrip_protocol_26() {
        let head = SumHead { blength: 700, s2length: 16, count: 10, remainder: 5 };
        let mut buf = Vec::new();
        head.write(&mut buf, 26).unwrap();
        assert_eq!(buf.len(), 12);
        let got = SumHead::read(&mut buf.as_slice(), 26).unwrap();
        assert_eq!(got.count,    10);
        assert_eq!(got.blength, 700);
        assert_eq!(got.s2length,  0);
        assert_eq!(got.remainder, 5);
    }

    #[test]
    fn sum_head_wire_is_little_endian() {
        let head = SumHead { blength: 0x0102_0304, s2length: 16, count: 0xFF, remainder: 1 };
        let mut buf = Vec::new();
        head.write(&mut buf, 30).unwrap();
        assert_eq!(&buf[0..4], &[0xFF, 0x00, 0x00, 0x00]);
        assert_eq!(&buf[4..8], &[0x04, 0x03, 0x02, 0x01]);
    }

    #[test]
    fn sum_head_for_file() {
        let h = SumHead::for_file(1400, 16, 30);
        assert_eq!(h.count,     2);
        assert_eq!(h.blength, 700);
        assert_eq!(h.remainder, 0);
        assert_eq!(h.s2length, 16);

        let h2 = SumHead::for_file(701, 16, 30);
        assert_eq!(h2.count,     2);
        assert_eq!(h2.remainder, 1);

        let h3 = SumHead::for_file(0, 16, 30);
        assert_eq!(h3.count, 0);
    }
}
