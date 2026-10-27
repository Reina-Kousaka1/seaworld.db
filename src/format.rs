use crate::{Error, Result};

pub(crate) const HEADER_SIZE: usize = 64;
pub(crate) const BLOCK_SIZE: u32 = 4096;
pub(crate) const RECORD_PREFIX_SIZE: usize = 20;
pub(crate) const RECORD_TRAILER_SIZE: usize = 4;
pub(crate) const MIN_RECORD_SIZE: usize = RECORD_PREFIX_SIZE + RECORD_TRAILER_SIZE;
pub(crate) const MAX_RECORD_SIZE: usize = 17 * 1024 * 1024;
pub(crate) const MAX_DATABASE_SIZE_BYTES: u64 = 128 * 1024 * 1024;
pub(crate) const FORMAT_VERSION: u16 = 1;
pub(crate) const TRANSACTION_FORMAT_VERSION: u16 = 2;
pub(crate) const MVCC_FORMAT_VERSION: u16 = 3;
pub(crate) const HEADER_MAGIC: &[u8; 8] = b"SWDB\r\n\x1a\n";
pub(crate) const RECORD_MAGIC: &[u8; 4] = b"SWBL";

#[derive(Clone, Copy)]
pub(crate) struct FileHeader {
    pub(crate) version: u16,
    pub(crate) database_id: [u8; 16],
}

pub(crate) fn encode_header(database_id: [u8; 16]) -> [u8; HEADER_SIZE] {
    encode_header_version(database_id, FORMAT_VERSION)
}

pub(crate) fn encode_header_version(mut database_id: [u8; 16], version: u16) -> [u8; HEADER_SIZE] {
    database_id[6] = (database_id[6] & 0x0f) | 0x40;
    database_id[8] = (database_id[8] & 0x3f) | 0x80;

    let mut header = [0_u8; HEADER_SIZE];
    header[..8].copy_from_slice(HEADER_MAGIC);
    header[8..10].copy_from_slice(&version.to_be_bytes());
    header[10..12].copy_from_slice(&(HEADER_SIZE as u16).to_be_bytes());
    header[12..16].copy_from_slice(&BLOCK_SIZE.to_be_bytes());
    header[24..40].copy_from_slice(&database_id);
    let checksum = crc32c(&header[..60]);
    header[60..64].copy_from_slice(&checksum.to_be_bytes());
    header
}

pub(crate) fn decode_header(bytes: &[u8]) -> Result<FileHeader> {
    if bytes.len() != HEADER_SIZE {
        return Err(Error::Format("header must be exactly 64 bytes".into()));
    }
    if &bytes[..8] != HEADER_MAGIC {
        return Err(Error::Format("invalid SeaWorld magic bytes".into()));
    }
    let version = u16::from_be_bytes(bytes[8..10].try_into().expect("fixed header range"));
    if version != FORMAT_VERSION
        && version != TRANSACTION_FORMAT_VERSION
        && version != MVCC_FORMAT_VERSION
    {
        return Err(Error::Format(format!(
            "unsupported format version {version}"
        )));
    }
    let header_len = u16::from_be_bytes(bytes[10..12].try_into().expect("fixed header range"));
    if usize::from(header_len) != HEADER_SIZE {
        return Err(Error::Format(format!(
            "unsupported header length {header_len}"
        )));
    }
    let block_size = u32::from_be_bytes(bytes[12..16].try_into().expect("fixed header range"));
    if block_size != BLOCK_SIZE {
        return Err(Error::Format(format!(
            "unsupported block size {block_size}"
        )));
    }
    let flags = u64::from_be_bytes(bytes[16..24].try_into().expect("fixed header range"));
    if flags != 0 {
        return Err(Error::Format("unsupported required feature flags".into()));
    }
    if bytes[40..60].iter().any(|byte| *byte != 0) {
        return Err(Error::Format("reserved header bytes must be zero".into()));
    }
    let database_id: [u8; 16] = bytes[24..40].try_into().expect("fixed header range");
    if database_id.iter().all(|byte| *byte == 0) {
        return Err(Error::Format("database identifier cannot be zero".into()));
    }
    let stored = u32::from_be_bytes(bytes[60..64].try_into().expect("fixed header range"));
    let actual = crc32c(&bytes[..60]);
    if stored != actual {
        return Err(Error::Integrity("header checksum mismatch".into()));
    }
    Ok(FileHeader {
        version,
        database_id,
    })
}

pub(crate) fn crc32c(bytes: &[u8]) -> u32 {
    let mut crc = !0_u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = 0_u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0x82f6_3b78 & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32c_matches_the_standard_check_value() {
        assert_eq!(crc32c(b"123456789"), 0xe306_9283);
    }

    #[test]
    fn header_round_trips_and_checks_reserved_bytes() {
        let header = encode_header([1; 16]);
        assert_eq!(
            decode_header(&header).unwrap().database_id[..6],
            [1, 1, 1, 1, 1, 1]
        );
        let mut bad = header;
        bad[40] = 1;
        let checksum = crc32c(&bad[..60]);
        bad[60..].copy_from_slice(&checksum.to_be_bytes());
        assert!(matches!(decode_header(&bad), Err(Error::Format(_))));
    }
}
