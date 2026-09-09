//! The `.flatbak` container: a fixed header, a compressed manifest, a
//! compressed payload and an integrity trailer.
//!
//! ```text
//! offset  size  field
//! 0       8     magic "FLATBAK\x1a"
//! 8       2     container_version   u16 LE
//! 10      2     reserved            u16 LE (0)
//! 12      4     manifest_len        u32 LE  compressed manifest length
//! 16      8     payload_len         u64 LE  compressed payload length
//! 24      4     header_crc32        u32 LE  CRC-32 of bytes 0..24
//! 28      4     reserved            u32 LE (0)
//! 32      N     manifest            zstd(TOML)
//! 32+N    M     payload             zstd(tar)
//! end-40  32    payload_sha256      SHA-256 of the *compressed* payload bytes
//! end-8   8     trailer magic "FBKEND\r\n"
//! ```
//!
//! The manifest sits at a known offset ahead of the payload, so opening a
//! multi-gigabyte backup to list its applications reads a few kilobytes rather
//! than decompressing the whole file. The trailer carries the checksum because
//! it can only be known once the payload has been written; a reader verifies it
//! while streaming the payload during a restore.
//!
//! `container_version` covers this framing, while the manifest's own
//! `format_version` covers its contents. They are separate so the framing can
//! stay put while the manifest evolves, which is the likelier change.

use std::io::{Read, Seek, SeekFrom, Write};

use sha2::{Digest as _, Sha256};

use crate::error::ArchiveError;

pub const MAGIC: [u8; 8] = *b"FLATBAK\x1a";
pub const TRAILER_MAGIC: [u8; 8] = *b"FBKEND\r\n";
pub const CONTAINER_VERSION: u16 = 1;
/// Highest framing version this build understands.
pub const CONTAINER_VERSION_MAX: u16 = 1;
pub const HEADER_LEN: u64 = 32;
pub const TRAILER_LEN: u64 = 40;

/// Fixed-size header at the start of every archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub container_version: u16,
    pub manifest_len: u32,
    pub payload_len: u64,
}

impl Header {
    pub fn new(manifest_len: u32, payload_len: u64) -> Self {
        Self {
            container_version: CONTAINER_VERSION,
            manifest_len,
            payload_len,
        }
    }

    pub fn encode(&self) -> [u8; HEADER_LEN as usize] {
        let mut bytes = [0u8; HEADER_LEN as usize];
        bytes[0..8].copy_from_slice(&MAGIC);
        bytes[8..10].copy_from_slice(&self.container_version.to_le_bytes());
        // 10..12 reserved
        bytes[12..16].copy_from_slice(&self.manifest_len.to_le_bytes());
        bytes[16..24].copy_from_slice(&self.payload_len.to_le_bytes());
        let crc = crc32fast::hash(&bytes[0..24]);
        bytes[24..28].copy_from_slice(&crc.to_le_bytes());
        // 28..32 reserved
        bytes
    }

    pub fn decode(bytes: &[u8; HEADER_LEN as usize]) -> Result<Self, ArchiveError> {
        if bytes[0..8] != MAGIC {
            // Signalled by the caller, which knows the path.
            return Err(ArchiveError::Corrupt("bad file signature".to_owned()));
        }
        let stored_crc = u32::from_le_bytes(bytes[24..28].try_into().expect("4 bytes"));
        if crc32fast::hash(&bytes[0..24]) != stored_crc {
            return Err(ArchiveError::Corrupt(
                "the file header failed its checksum".to_owned(),
            ));
        }
        let container_version = u16::from_le_bytes(bytes[8..10].try_into().expect("2 bytes"));
        if container_version > CONTAINER_VERSION_MAX {
            return Err(ArchiveError::UnsupportedVersion {
                found: container_version as u32,
                supported: CONTAINER_VERSION_MAX as u32,
            });
        }
        Ok(Self {
            container_version,
            manifest_len: u32::from_le_bytes(bytes[12..16].try_into().expect("4 bytes")),
            payload_len: u64::from_le_bytes(bytes[16..24].try_into().expect("8 bytes")),
        })
    }

    /// Byte offset of the payload within the file.
    pub fn payload_offset(&self) -> u64 {
        HEADER_LEN + self.manifest_len as u64
    }

    /// The exact file length this header implies.
    pub fn expected_file_len(&self) -> u64 {
        HEADER_LEN + self.manifest_len as u64 + self.payload_len + TRAILER_LEN
    }
}

/// Integrity trailer at the end of every archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Trailer {
    pub payload_sha256: [u8; 32],
}

impl Trailer {
    pub fn encode(&self) -> [u8; TRAILER_LEN as usize] {
        let mut bytes = [0u8; TRAILER_LEN as usize];
        bytes[0..32].copy_from_slice(&self.payload_sha256);
        bytes[32..40].copy_from_slice(&TRAILER_MAGIC);
        bytes
    }

    pub fn decode(bytes: &[u8; TRAILER_LEN as usize]) -> Result<Self, ArchiveError> {
        if bytes[32..40] != TRAILER_MAGIC {
            return Err(ArchiveError::Truncated);
        }
        let mut payload_sha256 = [0u8; 32];
        payload_sha256.copy_from_slice(&bytes[0..32]);
        Ok(Self { payload_sha256 })
    }
}

/// Reads the header from the start of `file`.
pub fn read_header<R: Read + Seek>(file: &mut R) -> Result<Header, ArchiveError> {
    file.seek(SeekFrom::Start(0))
        .map_err(|error| ArchiveError::Corrupt(error.to_string()))?;
    let mut bytes = [0u8; HEADER_LEN as usize];
    file.read_exact(&mut bytes).map_err(|error| {
        if error.kind() == std::io::ErrorKind::UnexpectedEof {
            ArchiveError::Truncated
        } else {
            ArchiveError::Corrupt(error.to_string())
        }
    })?;
    Header::decode(&bytes)
}

/// Reads the trailer from the end of `file`.
pub fn read_trailer<R: Read + Seek>(file: &mut R, file_len: u64) -> Result<Trailer, ArchiveError> {
    if file_len < HEADER_LEN + TRAILER_LEN {
        return Err(ArchiveError::Truncated);
    }
    file.seek(SeekFrom::Start(file_len - TRAILER_LEN))
        .map_err(|error| ArchiveError::Corrupt(error.to_string()))?;
    let mut bytes = [0u8; TRAILER_LEN as usize];
    file.read_exact(&mut bytes)
        .map_err(|_| ArchiveError::Truncated)?;
    Trailer::decode(&bytes)
}

/// Writes `header` at offset 0, used both to reserve space and to patch in the
/// payload length once it is known.
pub fn write_header<W: Write + Seek>(file: &mut W, header: &Header) -> std::io::Result<()> {
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&header.encode())
}

/// A writer that counts and hashes everything passing through it.
///
/// Wrapped *around* the compressor so the hash covers the compressed payload
/// exactly as it lands on disk, which is what a reader can cheaply verify.
pub struct HashingWriter<W> {
    inner: W,
    hasher: Sha256,
    written: u64,
}

impl<W: Write> HashingWriter<W> {
    pub fn new(inner: W) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
            written: 0,
        }
    }

    /// Returns the byte count and digest, plus the wrapped writer.
    pub fn finish(self) -> (W, u64, [u8; 32]) {
        let digest = self.hasher.finalize();
        let mut out = [0u8; 32];
        out.copy_from_slice(&digest);
        (self.inner, self.written, out)
    }
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let written = self.inner.write(buf)?;
        self.hasher.update(&buf[..written]);
        self.written += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// A reader that hashes everything passing through it and stops after `limit`
/// bytes, so the payload can be verified while it is being extracted.
pub struct HashingLimitReader<R> {
    inner: R,
    hasher: Sha256,
    remaining: u64,
}

impl<R: Read> HashingLimitReader<R> {
    pub fn new(inner: R, limit: u64) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
            remaining: limit,
        }
    }

    pub fn remaining(&self) -> u64 {
        self.remaining
    }

    pub fn finish(self) -> [u8; 32] {
        let digest = self.hasher.finalize();
        let mut out = [0u8; 32];
        out.copy_from_slice(&digest);
        out
    }
}

impl<R: Read> Read for HashingLimitReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.remaining == 0 {
            return Ok(0);
        }
        let cap = buf.len().min(self.remaining as usize);
        let read = self.inner.read(&mut buf[..cap])?;
        self.hasher.update(&buf[..read]);
        self.remaining -= read as u64;
        Ok(read)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_round_trips() {
        let header = Header::new(4096, 123_456_789);
        let decoded = Header::decode(&header.encode()).unwrap();
        assert_eq!(decoded, header);
        assert_eq!(decoded.payload_offset(), HEADER_LEN + 4096);
    }

    #[test]
    fn header_rejects_bad_magic() {
        let mut bytes = Header::new(1, 1).encode();
        bytes[0] = b'X';
        assert!(matches!(
            Header::decode(&bytes),
            Err(ArchiveError::Corrupt(_))
        ));
    }

    #[test]
    fn header_rejects_bit_flips() {
        let mut bytes = Header::new(4096, 99).encode();
        bytes[16] ^= 0x01;
        assert!(matches!(
            Header::decode(&bytes),
            Err(ArchiveError::Corrupt(_))
        ));
    }

    #[test]
    fn header_rejects_future_framing() {
        let mut bytes = Header::new(1, 1).encode();
        bytes[8..10].copy_from_slice(&(CONTAINER_VERSION_MAX + 1).to_le_bytes());
        let crc = crc32fast::hash(&bytes[0..24]);
        bytes[24..28].copy_from_slice(&crc.to_le_bytes());
        assert!(matches!(
            Header::decode(&bytes),
            Err(ArchiveError::UnsupportedVersion { .. })
        ));
    }

    #[test]
    fn trailer_round_trips() {
        let trailer = Trailer {
            payload_sha256: [7u8; 32],
        };
        assert_eq!(Trailer::decode(&trailer.encode()).unwrap(), trailer);
    }

    #[test]
    fn trailer_rejects_missing_magic() {
        let bytes = [0u8; TRAILER_LEN as usize];
        assert!(matches!(
            Trailer::decode(&bytes),
            Err(ArchiveError::Truncated)
        ));
    }

    #[test]
    fn hashing_writer_counts_and_hashes() {
        let mut writer = HashingWriter::new(Vec::new());
        writer.write_all(b"hello world").unwrap();
        let (inner, written, digest) = writer.finish();
        assert_eq!(inner, b"hello world");
        assert_eq!(written, 11);

        let expected = Sha256::digest(b"hello world");
        assert_eq!(&digest[..], &expected[..]);
    }

    #[test]
    fn limit_reader_stops_at_limit() {
        let data = b"0123456789".to_vec();
        let mut reader = HashingLimitReader::new(data.as_slice(), 4);
        let mut out = Vec::new();
        reader.read_to_end(&mut out).unwrap();
        assert_eq!(out, b"0123");
        assert_eq!(reader.remaining(), 0);
        let expected = Sha256::digest(b"0123");
        assert_eq!(&reader.finish()[..], &expected[..]);
    }
}
