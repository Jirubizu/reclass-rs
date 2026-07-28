//! The minimum ZIP needed for `.rcnet`: read one named entry, write one.
//!
//! A `.rcnet` is a ZIP holding a single `Data.xml`, written by .NET's
//! `ZipArchive`. That is all this handles — no multi-disk, no encryption, no
//! ZIP64, no directory traversal, because the format never uses them. A general
//! ZIP crate would be a dependency for a one-entry container.

use std::io::{Read, Write};

use super::RcnetError;

const LOCAL_SIG: u32 = 0x0403_4B50;
const CENTRAL_SIG: u32 = 0x0201_4B50;
const EOCD_SIG: u32 = 0x0605_4B50;
/// The end-of-central-directory record, without a trailing comment.
const EOCD_LEN: usize = 22;

const STORED: u16 = 0;
const DEFLATED: u16 = 8;

fn u16_at(b: &[u8], off: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(off..off + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(off..off + 4)?.try_into().ok()?))
}

/// Extract the entry named `want` from a ZIP archive in memory.
///
/// Reads the central directory rather than scanning local headers: a local
/// header may carry a zeroed size with the real values in a trailing data
/// descriptor, which is exactly what a streaming writer like .NET's emits.
pub(super) fn read_entry(zip: &[u8], want: &str) -> Result<Vec<u8>, RcnetError> {
    let eocd = find_eocd(zip).ok_or(RcnetError::NotAZip)?;
    let entries = u16_at(zip, eocd + 10).ok_or(RcnetError::NotAZip)?;
    let mut off = u32_at(zip, eocd + 16).ok_or(RcnetError::NotAZip)? as usize;

    for _ in 0..entries {
        if u32_at(zip, off) != Some(CENTRAL_SIG) {
            return Err(RcnetError::NotAZip);
        }
        let method = u16_at(zip, off + 10).ok_or(RcnetError::NotAZip)?;
        let compressed = u32_at(zip, off + 20).ok_or(RcnetError::NotAZip)? as usize;
        let uncompressed = u32_at(zip, off + 24).ok_or(RcnetError::NotAZip)? as usize;
        let name_len = u16_at(zip, off + 28).ok_or(RcnetError::NotAZip)? as usize;
        let extra_len = u16_at(zip, off + 30).ok_or(RcnetError::NotAZip)? as usize;
        let comment_len = u16_at(zip, off + 32).ok_or(RcnetError::NotAZip)? as usize;
        let local_off = u32_at(zip, off + 42).ok_or(RcnetError::NotAZip)? as usize;
        let name = zip
            .get(off + 46..off + 46 + name_len)
            .ok_or(RcnetError::NotAZip)?;

        if name.eq_ignore_ascii_case(want.as_bytes()) {
            return read_local(zip, local_off, method, compressed, uncompressed);
        }
        off += 46 + name_len + extra_len + comment_len;
    }
    Err(RcnetError::MissingEntry(want.to_string()))
}

/// Locate the end-of-central-directory record.
///
/// Scanned backwards because the record is last and may be followed by a
/// variable-length comment; the 64 KiB bound is the largest comment ZIP allows.
fn find_eocd(zip: &[u8]) -> Option<usize> {
    let max_back = zip.len().min(EOCD_LEN + u16::MAX as usize);
    let start = zip.len().checked_sub(max_back)?;
    (start..=zip.len().checked_sub(EOCD_LEN)?)
        .rev()
        .find(|&i| u32_at(zip, i) == Some(EOCD_SIG))
}

/// Read and decompress an entry's payload given its local-header offset.
///
/// The local header's own name/extra lengths are used to find the payload —
/// they can differ from the central directory's, and only the local ones
/// describe the bytes that actually follow.
fn read_local(
    zip: &[u8],
    local_off: usize,
    method: u16,
    compressed: usize,
    uncompressed: usize,
) -> Result<Vec<u8>, RcnetError> {
    if u32_at(zip, local_off) != Some(LOCAL_SIG) {
        return Err(RcnetError::NotAZip);
    }
    let name_len = u16_at(zip, local_off + 26).ok_or(RcnetError::NotAZip)? as usize;
    let extra_len = u16_at(zip, local_off + 28).ok_or(RcnetError::NotAZip)? as usize;
    let start = local_off + 30 + name_len + extra_len;
    let data = zip
        .get(start..start + compressed)
        .ok_or(RcnetError::NotAZip)?;

    match method {
        STORED => Ok(data.to_vec()),
        DEFLATED => {
            let mut out = Vec::with_capacity(uncompressed);
            flate2::read::DeflateDecoder::new(data)
                .read_to_end(&mut out)
                .map_err(|e| RcnetError::Corrupt(e.to_string()))?;
            Ok(out)
        }
        other => Err(RcnetError::Compression(other)),
    }
}

/// Build a one-entry ZIP archive holding `data` under `name`.
///
/// Deflated rather than stored: `Data.xml` is highly repetitive markup, and
/// .NET's reader handles both anyway.
pub(super) fn write_entry(name: &str, data: &[u8]) -> Result<Vec<u8>, RcnetError> {
    let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(data)
        .and_then(|()| enc.try_finish())
        .map_err(|e| RcnetError::Corrupt(e.to_string()))?;
    let body = enc
        .finish()
        .map_err(|e| RcnetError::Corrupt(e.to_string()))?;

    let mut crc = flate2::Crc::new();
    crc.update(data);
    let crc = crc.sum();

    let name_b = name.as_bytes();
    let nlen =
        u16::try_from(name_b.len()).map_err(|_| RcnetError::Corrupt("name too long".into()))?;
    let clen =
        u32::try_from(body.len()).map_err(|_| RcnetError::Corrupt("entry too large".into()))?;
    let ulen =
        u32::try_from(data.len()).map_err(|_| RcnetError::Corrupt("entry too large".into()))?;

    let mut out = Vec::with_capacity(body.len() + 128);
    // -- local file header --
    out.extend_from_slice(&LOCAL_SIG.to_le_bytes());
    out.extend_from_slice(&20u16.to_le_bytes()); // version needed
    out.extend_from_slice(&0u16.to_le_bytes()); // flags
    out.extend_from_slice(&DEFLATED.to_le_bytes());
    // No timestamp: a zeroed DOS time keeps the output byte-identical for the
    // same input, which makes the round-trip test meaningful.
    out.extend_from_slice(&0u16.to_le_bytes()); // mod time
    out.extend_from_slice(&0u16.to_le_bytes()); // mod date
    out.extend_from_slice(&crc.to_le_bytes());
    out.extend_from_slice(&clen.to_le_bytes());
    out.extend_from_slice(&ulen.to_le_bytes());
    out.extend_from_slice(&nlen.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // extra len
    out.extend_from_slice(name_b);
    out.extend_from_slice(&body);

    // -- central directory --
    let cd_start = out.len();
    out.extend_from_slice(&CENTRAL_SIG.to_le_bytes());
    out.extend_from_slice(&20u16.to_le_bytes()); // version made by
    out.extend_from_slice(&20u16.to_le_bytes()); // version needed
    out.extend_from_slice(&0u16.to_le_bytes()); // flags
    out.extend_from_slice(&DEFLATED.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // mod time
    out.extend_from_slice(&0u16.to_le_bytes()); // mod date
    out.extend_from_slice(&crc.to_le_bytes());
    out.extend_from_slice(&clen.to_le_bytes());
    out.extend_from_slice(&ulen.to_le_bytes());
    out.extend_from_slice(&nlen.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // extra len
    out.extend_from_slice(&0u16.to_le_bytes()); // comment len
    out.extend_from_slice(&0u16.to_le_bytes()); // disk number
    out.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
    out.extend_from_slice(&0u32.to_le_bytes()); // external attrs
    out.extend_from_slice(&0u32.to_le_bytes()); // local header offset
    out.extend_from_slice(name_b);
    let cd_len = out.len() - cd_start;

    // -- end of central directory --
    out.extend_from_slice(&EOCD_SIG.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // this disk
    out.extend_from_slice(&0u16.to_le_bytes()); // cd start disk
    out.extend_from_slice(&1u16.to_le_bytes()); // entries on this disk
    out.extend_from_slice(&1u16.to_le_bytes()); // entries total
    out.extend_from_slice(&(cd_len as u32).to_le_bytes());
    out.extend_from_slice(&(cd_start as u32).to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // comment len
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_written_entry_reads_back() {
        let payload = b"<reclass version=\"65537\"/>".repeat(50);
        let zip = write_entry("Data.xml", &payload).unwrap();
        assert_eq!(read_entry(&zip, "Data.xml").unwrap(), payload);
        // deflate actually happened on this repetitive input
        assert!(zip.len() < payload.len(), "entry was not compressed");
    }

    #[test]
    fn entry_names_match_case_insensitively() {
        // .NET writes "Data.xml"; some tooling lowercases it.
        let zip = write_entry("data.xml", b"x").unwrap();
        assert_eq!(read_entry(&zip, "Data.xml").unwrap(), b"x");
    }

    #[test]
    fn a_missing_entry_names_itself() {
        let zip = write_entry("Data.xml", b"x").unwrap();
        assert!(matches!(
            read_entry(&zip, "Other.xml"),
            Err(RcnetError::MissingEntry(n)) if n == "Other.xml"
        ));
    }

    #[test]
    fn garbage_is_rejected_rather_than_panicking() {
        for bytes in [
            b"".to_vec(),
            b"not a zip at all".to_vec(),
            vec![0u8; EOCD_LEN],
        ] {
            assert!(matches!(
                read_entry(&bytes, "Data.xml"),
                Err(RcnetError::NotAZip)
            ));
        }
    }

    #[test]
    fn a_truncated_archive_errors_instead_of_slicing_out_of_range() {
        let zip = write_entry("Data.xml", &b"payload".repeat(100)).unwrap();
        // keep the EOCD (it is at the end) but cut the payload out from under it
        let mut broken = zip.clone();
        broken.drain(40..80);
        assert!(read_entry(&broken, "Data.xml").is_err());
    }

    #[test]
    fn writing_is_deterministic() {
        // No timestamp, so the same input gives the same bytes — otherwise a
        // round-trip test could not compare archives.
        let a = write_entry("Data.xml", b"same").unwrap();
        let b = write_entry("Data.xml", b"same").unwrap();
        assert_eq!(a, b);
    }
}
