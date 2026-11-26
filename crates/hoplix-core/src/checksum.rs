use ring::digest;
use tokio::io::AsyncReadExt;

use crate::key_value::{from_hex, to_hex};
use crate::{Error, ErrorKind, FileSystem, Result};

// Constants

const BUFFER_SIZE: usize = 128 * 1024;
const CRC_POLYNOMIAL: u32 = 0xedb8_8320;
const CRC_TABLE: [u32; 256] = crc_table();
const MD5_SHIFTS: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9,
    14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15,
    21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];
const MD5_CONSTANTS: [u32; 64] = [
    0xd76a_a478,
    0xe8c7_b756,
    0x2420_70db,
    0xc1bd_ceee,
    0xf57c_0faf,
    0x4787_c62a,
    0xa830_4613,
    0xfd46_9501,
    0x6980_98d8,
    0x8b44_f7af,
    0xffff_5bb1,
    0x895c_d7be,
    0x6b90_1122,
    0xfd98_7193,
    0xa679_438e,
    0x49b4_0821,
    0xf61e_2562,
    0xc040_b340,
    0x265e_5a51,
    0xe9b6_c7aa,
    0xd62f_105d,
    0x0244_1453,
    0xd8a1_e681,
    0xe7d3_fbc8,
    0x21e1_cde6,
    0xc337_07d6,
    0xf4d5_0d87,
    0x455a_14ed,
    0xa9e3_e905,
    0xfcef_a3f8,
    0x676f_02d9,
    0x8d2a_4c8a,
    0xfffa_3942,
    0x8771_f681,
    0x6d9d_6122,
    0xfde5_380c,
    0xa4be_ea44,
    0x4bde_cfa9,
    0xf6bb_4b60,
    0xbebf_bc70,
    0x289b_7ec6,
    0xeaa1_27fa,
    0xd4ef_3085,
    0x0488_1d05,
    0xd9d4_d039,
    0xe6db_99e5,
    0x1fa2_7cf8,
    0xc4ac_5665,
    0xf429_2244,
    0x432a_ff97,
    0xab94_23a7,
    0xfc93_a039,
    0x655b_59c3,
    0x8f0c_cc92,
    0xffef_f47d,
    0x8584_5dd1,
    0x6fa8_7e4f,
    0xfe2c_e6e0,
    0xa301_4314,
    0x4e08_11a1,
    0xf753_7e82,
    0xbd3a_f235,
    0x2ad7_d2bb,
    0xeb86_d391,
];

// Types

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HashAlgorithm {
    Sha256,
    Sha1,
    Md5,
    Crc32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChecksumMode {
    Off,
    #[default]
    WhenSupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verification {
    Matched(HashAlgorithm),
    SizeChecked,
    Mismatch(Option<HashAlgorithm>),
}

pub struct Hasher {
    state: HasherState,
}

enum HasherState {
    Digest(digest::Context),
    Md5(Md5),
    Crc32(u32),
}

struct Md5 {
    state: [u32; 4],
    pending: Vec<u8>,
    length: u64,
}

// Algorithms

impl HashAlgorithm {
    pub const PREFERENCE: [Self; 4] = [Self::Sha256, Self::Sha1, Self::Md5, Self::Crc32];

    pub fn name(self) -> &'static str {
        match self {
            Self::Sha256 => "SHA-256",
            Self::Sha1 => "SHA-1",
            Self::Md5 => "MD5",
            Self::Crc32 => "CRC32",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Sha256 => "sha256",
            Self::Sha1 => "sha1",
            Self::Md5 => "md5",
            Self::Crc32 => "crc32",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::PREFERENCE
            .into_iter()
            .find(|algorithm| algorithm.key() == key)
    }

    pub fn from_name(name: &str) -> Option<Self> {
        let name = name.trim().to_ascii_uppercase().replace('_', "-");
        match name.as_str() {
            "SHA-256" | "SHA256" => Some(Self::Sha256),
            "SHA-1" | "SHA1" => Some(Self::Sha1),
            "MD5" => Some(Self::Md5),
            "CRC32" | "CRC-32" | "CRC" => Some(Self::Crc32),
            _ => None,
        }
    }

    pub fn digest(self, data: &[u8]) -> Vec<u8> {
        let mut hasher = Hasher::new(self);
        hasher.update(data);
        hasher.finish()
    }

    pub fn best_common(left: &[Self], right: &[Self]) -> Option<Self> {
        Self::PREFERENCE
            .into_iter()
            .find(|algorithm| left.contains(algorithm) && right.contains(algorithm))
    }
}

impl ChecksumMode {
    pub const ALL: [Self; 2] = [Self::Off, Self::WhenSupported];

    pub fn key(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::WhenSupported => "supported",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.key() == key)
    }
}

impl Verification {
    pub fn is_mismatch(self) -> bool {
        matches!(self, Self::Mismatch(_))
    }

    pub fn combine(self, other: Self) -> Self {
        match (self, other) {
            (Self::Mismatch(algorithm), _) | (_, Self::Mismatch(algorithm)) => {
                Self::Mismatch(algorithm)
            }
            (Self::SizeChecked, _) | (_, Self::SizeChecked) => Self::SizeChecked,
            (Self::Matched(first), Self::Matched(second)) if first == second => {
                Self::Matched(first)
            }
            (Self::Matched(first), Self::Matched(second)) => {
                let weaker = HashAlgorithm::PREFERENCE
                    .into_iter()
                    .rev()
                    .find(|algorithm| *algorithm == first || *algorithm == second)
                    .unwrap_or(first);
                Self::Matched(weaker)
            }
        }
    }

    pub fn key(self) -> String {
        match self {
            Self::Matched(algorithm) => format!("matched:{}", algorithm.key()),
            Self::SizeChecked => "size".to_string(),
            Self::Mismatch(Some(algorithm)) => format!("mismatch:{}", algorithm.key()),
            Self::Mismatch(None) => "mismatch:size".to_string(),
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        match key.split_once(':') {
            None if key == "size" => Some(Self::SizeChecked),
            Some(("matched", algorithm)) => HashAlgorithm::from_key(algorithm).map(Self::Matched),
            Some(("mismatch", "size")) => Some(Self::Mismatch(None)),
            Some(("mismatch", algorithm)) => {
                HashAlgorithm::from_key(algorithm).map(|found| Self::Mismatch(Some(found)))
            }
            _ => None,
        }
    }
}

// Hashing

impl Hasher {
    pub fn new(algorithm: HashAlgorithm) -> Self {
        let state = match algorithm {
            HashAlgorithm::Sha256 => HasherState::Digest(digest::Context::new(&digest::SHA256)),
            HashAlgorithm::Sha1 => {
                HasherState::Digest(digest::Context::new(&digest::SHA1_FOR_LEGACY_USE_ONLY))
            }
            HashAlgorithm::Md5 => HasherState::Md5(Md5::new()),
            HashAlgorithm::Crc32 => HasherState::Crc32(u32::MAX),
        };
        Self { state }
    }

    pub fn update(&mut self, data: &[u8]) {
        match &mut self.state {
            HasherState::Digest(context) => context.update(data),
            HasherState::Md5(md5) => md5.update(data),
            HasherState::Crc32(crc) => {
                for byte in data {
                    *crc = CRC_TABLE[((*crc ^ u32::from(*byte)) & 0xff) as usize] ^ (*crc >> 8);
                }
            }
        }
    }

    pub fn finish(self) -> Vec<u8> {
        match self.state {
            HasherState::Digest(context) => context.finish().as_ref().to_vec(),
            HasherState::Md5(md5) => md5.finish().to_vec(),
            HasherState::Crc32(crc) => (!crc).to_be_bytes().to_vec(),
        }
    }
}

impl Md5 {
    fn new() -> Self {
        Self {
            state: [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476],
            pending: Vec::with_capacity(64),
            length: 0,
        }
    }

    fn update(&mut self, mut data: &[u8]) {
        self.length = self.length.wrapping_add(data.len() as u64);
        if !self.pending.is_empty() {
            let needed = 64 - self.pending.len();
            let taken = needed.min(data.len());
            self.pending.extend_from_slice(&data[..taken]);
            data = &data[taken..];
            if self.pending.len() < 64 {
                return;
            }
            let block = std::mem::take(&mut self.pending);
            self.process(&block);
        }
        let mut blocks = data.chunks_exact(64);
        for block in &mut blocks {
            self.process(block);
        }
        self.pending.extend_from_slice(blocks.remainder());
    }

    fn process(&mut self, block: &[u8]) {
        let mut words = [0u32; 16];
        for (word, bytes) in words.iter_mut().zip(block.chunks_exact(4)) {
            *word = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        }
        let [mut a, mut b, mut c, mut d] = self.state;
        for index in 0..64 {
            let (mixed, word) = match index / 16 {
                0 => ((b & c) | (!b & d), index),
                1 => ((d & b) | (!d & c), (5 * index + 1) % 16),
                2 => (b ^ c ^ d, (3 * index + 5) % 16),
                _ => (c ^ (b | !d), (7 * index) % 16),
            };
            let sum = mixed
                .wrapping_add(a)
                .wrapping_add(MD5_CONSTANTS[index])
                .wrapping_add(words[word]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(sum.rotate_left(MD5_SHIFTS[index]));
        }
        for (state, value) in self.state.iter_mut().zip([a, b, c, d]) {
            *state = state.wrapping_add(value);
        }
    }

    fn finish(mut self) -> [u8; 16] {
        let bits = self.length.wrapping_mul(8);
        let mut tail = std::mem::take(&mut self.pending);
        tail.push(0x80);
        while tail.len() % 64 != 56 {
            tail.push(0);
        }
        tail.extend_from_slice(&bits.to_le_bytes());
        for block in tail.chunks_exact(64) {
            self.process(block);
        }
        let mut output = [0u8; 16];
        for (chunk, word) in output.chunks_exact_mut(4).zip(self.state) {
            chunk.copy_from_slice(&word.to_le_bytes());
        }
        output
    }
}

const fn crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut index = 0;
    while index < 256 {
        let mut value = index as u32;
        let mut bit = 0;
        while bit < 8 {
            value = if value & 1 == 1 {
                CRC_POLYNOMIAL ^ (value >> 1)
            } else {
                value >> 1
            };
            bit += 1;
        }
        table[index] = value;
        index += 1;
    }
    table
}

// Hex

pub fn hex(bytes: &[u8]) -> String {
    to_hex(bytes)
}

pub fn parse_hex_digest(text: &str, algorithm: HashAlgorithm) -> Option<Vec<u8>> {
    let text = text.trim().to_ascii_lowercase();
    if algorithm == HashAlgorithm::Crc32 {
        let digits = text.strip_prefix("0x").unwrap_or(&text);
        if digits.is_empty() || digits.len() > 8 {
            return None;
        }
        return u32::from_str_radix(digits, 16)
            .ok()
            .map(|value| value.to_be_bytes().to_vec());
    }
    from_hex(&text).filter(|bytes| bytes.len() == digest_length(algorithm))
}

fn digest_length(algorithm: HashAlgorithm) -> usize {
    match algorithm {
        HashAlgorithm::Sha256 => 32,
        HashAlgorithm::Sha1 => 20,
        HashAlgorithm::Md5 => 16,
        HashAlgorithm::Crc32 => 4,
    }
}

// Files

pub async fn hash_file(
    file_system: &dyn FileSystem,
    path: &str,
    algorithm: HashAlgorithm,
) -> Result<Vec<u8>> {
    let mut reader = file_system.open_read(path, 0).await?;
    let mut hasher = Hasher::new(algorithm);
    let mut buffer = vec![0; BUFFER_SIZE];
    loop {
        let read = reader
            .read(&mut buffer)
            .await
            .map_err(|error| Error::from_io(&error, path))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finish())
}

pub async fn checksum_algorithms(file_system: &dyn FileSystem) -> Vec<HashAlgorithm> {
    if file_system.is_remote() {
        file_system.checksum_algorithms().await
    } else {
        HashAlgorithm::PREFERENCE.to_vec()
    }
}

pub async fn file_checksum(
    file_system: &dyn FileSystem,
    path: &str,
    algorithm: HashAlgorithm,
) -> Result<Vec<u8>> {
    if file_system.is_remote() {
        file_system.checksum(path, algorithm).await
    } else {
        hash_file(file_system, path, algorithm).await
    }
}

pub async fn verify_copy(
    source: &dyn FileSystem,
    source_path: &str,
    target: &dyn FileSystem,
    target_path: &str,
    size: u64,
) -> Result<Verification> {
    let copied = target.metadata(target_path).await?;
    if copied.size.is_some_and(|copied| copied != size) {
        return Ok(Verification::Mismatch(None));
    }
    let source_algorithms = checksum_algorithms(source).await;
    let target_algorithms = checksum_algorithms(target).await;
    let candidates = HashAlgorithm::PREFERENCE.into_iter().filter(|algorithm| {
        source_algorithms.contains(algorithm) && target_algorithms.contains(algorithm)
    });
    for algorithm in candidates {
        let source_hash = file_checksum(source, source_path, algorithm).await;
        let target_hash = file_checksum(target, target_path, algorithm).await;
        match (source_hash, target_hash) {
            (Ok(first), Ok(second)) if first == second => {
                return Ok(Verification::Matched(algorithm));
            }
            (Ok(_), Ok(_)) => return Ok(Verification::Mismatch(Some(algorithm))),
            (Err(error), _) | (_, Err(error)) if error.kind() == ErrorKind::NotFound => {
                return Err(error);
            }
            _ => {}
        }
    }
    Ok(Verification::SizeChecked)
}

pub fn unsupported(path: &str) -> Error {
    Error::new(ErrorKind::Other, format!("{path}: checksums unsupported"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LocalFileSystem;

    fn digest_hex(algorithm: HashAlgorithm, data: &[u8]) -> String {
        hex(&algorithm.digest(data))
    }

    #[test]
    fn md5_matches_the_rfc_test_vectors() {
        for (input, expected) in [
            ("", "d41d8cd98f00b204e9800998ecf8427e"),
            ("a", "0cc175b9c0f1b6a831c399e269772661"),
            ("abc", "900150983cd24fb0d6963f7d28e17f72"),
            ("message digest", "f96b697d7cb7938d525a2f31aaf161d0"),
            (
                "abcdefghijklmnopqrstuvwxyz",
                "c3fcd3d76192e4007dfb496cca67e13b",
            ),
            (
                "12345678901234567890123456789012345678901234567890123456789012345678901234567890",
                "57edf4a22be3c955ac49da2e2107b67a",
            ),
        ] {
            assert_eq!(digest_hex(HashAlgorithm::Md5, input.as_bytes()), expected);
        }
    }

    #[test]
    fn md5_handles_split_updates() {
        let data: Vec<u8> = (0..1000u32).map(|index| (index % 251) as u8).collect();
        let whole = HashAlgorithm::Md5.digest(&data);
        for split in [1, 55, 56, 63, 64, 65, 129, 999] {
            let mut hasher = Hasher::new(HashAlgorithm::Md5);
            for part in data.chunks(split) {
                hasher.update(part);
            }
            assert_eq!(hasher.finish(), whole, "{split}");
        }
    }

    #[test]
    fn crc32_sha1_and_sha256_match_known_vectors() {
        assert_eq!(digest_hex(HashAlgorithm::Crc32, b"123456789"), "cbf43926");
        assert_eq!(
            digest_hex(
                HashAlgorithm::Crc32,
                b"The quick brown fox jumps over the lazy dog"
            ),
            "414fa339"
        );
        assert_eq!(digest_hex(HashAlgorithm::Crc32, b""), "00000000");
        assert_eq!(
            digest_hex(HashAlgorithm::Sha1, b"abc"),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert_eq!(
            digest_hex(HashAlgorithm::Sha256, b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            digest_hex(HashAlgorithm::Sha256, b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn hex_digests_parse_per_algorithm() {
        assert_eq!(
            parse_hex_digest("CBF43926", HashAlgorithm::Crc32),
            Some(vec![0xcb, 0xf4, 0x39, 0x26])
        );
        assert_eq!(
            parse_hex_digest("1a2b", HashAlgorithm::Crc32),
            Some(vec![0, 0, 0x1a, 0x2b])
        );
        assert_eq!(
            parse_hex_digest("900150983CD24FB0D6963F7D28E17F72", HashAlgorithm::Md5),
            Some(HashAlgorithm::Md5.digest(b"abc"))
        );
        assert_eq!(parse_hex_digest("abcd", HashAlgorithm::Md5), None);
        assert_eq!(parse_hex_digest("zz", HashAlgorithm::Sha1), None);
    }

    #[test]
    fn names_keys_and_results_round_trip() {
        for algorithm in HashAlgorithm::PREFERENCE {
            assert_eq!(HashAlgorithm::from_key(algorithm.key()), Some(algorithm));
            assert_eq!(HashAlgorithm::from_name(algorithm.name()), Some(algorithm));
        }
        assert_eq!(
            HashAlgorithm::from_name("sha_256"),
            Some(HashAlgorithm::Sha256)
        );
        for verification in [
            Verification::Matched(HashAlgorithm::Md5),
            Verification::SizeChecked,
            Verification::Mismatch(None),
            Verification::Mismatch(Some(HashAlgorithm::Sha1)),
        ] {
            assert_eq!(
                Verification::from_key(&verification.key()),
                Some(verification)
            );
        }
        assert_eq!(Verification::from_key("matched:whirlpool"), None);
        assert_eq!(
            HashAlgorithm::best_common(
                &[HashAlgorithm::Md5, HashAlgorithm::Sha1],
                &HashAlgorithm::PREFERENCE
            ),
            Some(HashAlgorithm::Sha1)
        );
        assert_eq!(
            Verification::Matched(HashAlgorithm::Sha256)
                .combine(Verification::Matched(HashAlgorithm::Md5)),
            Verification::Matched(HashAlgorithm::Md5)
        );
        assert_eq!(
            Verification::Matched(HashAlgorithm::Sha256).combine(Verification::SizeChecked),
            Verification::SizeChecked
        );
        assert!(
            Verification::SizeChecked
                .combine(Verification::Mismatch(None))
                .is_mismatch()
        );
    }

    #[tokio::test]
    async fn local_copies_verify_with_sha256() {
        let directory =
            std::env::temp_dir().join(format!("hoplix-checksum-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("creating failed");
        let first = directory.join("first.bin");
        let second = directory.join("second.bin");
        std::fs::write(&first, b"same content").expect("writing failed");
        std::fs::write(&second, b"same content").expect("writing failed");
        let local = LocalFileSystem;
        let (first, second) = (
            first.to_string_lossy().into_owned(),
            second.to_string_lossy().into_owned(),
        );
        assert_eq!(
            verify_copy(&local, &first, &local, &second, 12).await,
            Ok(Verification::Matched(HashAlgorithm::Sha256))
        );
        std::fs::write(&second, b"same CONTENT").expect("writing failed");
        assert_eq!(
            verify_copy(&local, &first, &local, &second, 12).await,
            Ok(Verification::Mismatch(Some(HashAlgorithm::Sha256)))
        );
        std::fs::write(&second, b"short").expect("writing failed");
        assert_eq!(
            verify_copy(&local, &first, &local, &second, 12).await,
            Ok(Verification::Mismatch(None))
        );
        assert_eq!(
            hash_file(&local, &first, HashAlgorithm::Md5).await,
            Ok(HashAlgorithm::Md5.digest(b"same content"))
        );
        std::fs::remove_dir_all(directory).expect("cleaning up failed");
    }
}
