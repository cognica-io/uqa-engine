//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A current revision stores its significant big-endian bytes. Inline values derive their certified length from their representation; external records carry the original length and SHA-256.

use uqa_storage::{
    backup::{ImageDigest, RecordImageHasher, UnitFingerprint},
    mvcc::{CommitSequence, VersionError, VersionResult},
};

const INDEXED_LIVE: u8 = 0x10;
const EXTERNAL: u8 = 0x80;
const UNCERTIFIED: u8 = 0x40;
pub(in crate::mvcc) const INLINE_LIMIT: usize = 512;

pub(super) struct Revision {
    bytes: [u8; 8],
    width: u8,
}

impl Revision {
    pub(super) fn new(number: u64) -> Self {
        Self {
            bytes: number.to_be_bytes(),
            width: u8::try_from(8 - number.leading_zeros() / 8).expect("u64 width"),
        }
    }

    fn write(&self, output: &mut [u8], storage: u8) -> usize {
        let end = usize::from(self.width) + 1;
        output[0] = storage | self.width;
        output[1..end].copy_from_slice(&self.bytes[8 - usize::from(self.width)..]);
        end
    }

    pub(super) fn size(&self, length: Option<usize>) -> VersionResult<usize> {
        let payload = length.unwrap_or_default();
        let prefix = if payload <= INLINE_LIMIT {
            usize::from(self.width) + 1 + if payload > 0 { 32 } else { 0 }
        } else {
            0
        };
        payload
            .checked_add(prefix + 1)
            .ok_or(VersionError::InvalidEncoding("record size overflow"))
    }

    pub(super) fn inline(&self, output: &mut [u8], digest: Option<ImageDigest>) {
        let end = self.write(output, 0);
        if let Some(digest) = digest {
            output[end..end + 32].copy_from_slice(&digest.as_bytes());
        }
    }

    pub(super) fn indexed_live<'a>(&self, output: &'a mut [u8; 9]) -> &'a [u8] {
        let end = self.write(output, INDEXED_LIVE);
        &output[..end]
    }

    pub(super) fn external<'a>(
        &self,
        certificate: Option<&[u8; 40]>,
        output: &'a mut [u8; 49],
    ) -> &'a [u8] {
        if let Some(certificate) = certificate {
            let end = self.write(output, EXTERNAL);
            output[end..end + 40].copy_from_slice(certificate);
            &output[..end + 40]
        } else {
            let end = self.write(output, UNCERTIFIED);
            &output[..end]
        }
    }
}

pub(super) fn encode_certificate(length: usize, digest: ImageDigest) -> VersionResult<[u8; 40]> {
    let encoded = u64::try_from(length)
        .ok()
        .and_then(|n| n.checked_add(8))
        .ok_or(VersionError::InvalidEncoding(
            "record image length overflow",
        ))?;
    let mut certificate = [0; 40];
    certificate[..8].copy_from_slice(&encoded.to_be_bytes());
    certificate[8..].copy_from_slice(&digest.as_bytes());
    Ok(certificate)
}

pub(in crate::mvcc) struct RecordHead<'a> {
    pub(in crate::mvcc) revision: u64,
    pub(in crate::mvcc) certificate: Option<[u8; 40]>,
    pub(in crate::mvcc) certificate_bytes: u8,
    pub(in crate::mvcc) indexed_live: bool,
    pub(in crate::mvcc) inline: Option<&'a [u8]>,
}

impl<'a> RecordHead<'a> {
    pub(in crate::mvcc) fn live_empty(&self) -> bool {
        self.inline.is_some_and(|value| value == [1])
    }

    pub(in crate::mvcc) fn history_certificate(&self) -> VersionResult<Option<[u8; 40]>> {
        if self.live_empty() {
            return Ok(Some(encode_certificate(
                0,
                empty_fingerprint(self.revision).digest,
            )?));
        }
        Ok(self.certificate)
    }

    pub(in crate::mvcc) fn decode(bytes: &'a [u8]) -> VersionResult<Self> {
        let (&tag, body) = bytes
            .split_first()
            .ok_or(VersionError::InvalidEncoding("empty record head"))?;
        let layout = tag & 0xf0;
        let width = usize::from(tag & 0x0f);
        if !matches!(layout, 0 | INDEXED_LIVE | EXTERNAL | UNCERTIFIED) {
            return Err(VersionError::InvalidEncoding("unknown record head layout"));
        }
        if width > 8 || width != 0 && body.first() == Some(&0) {
            return Err(VersionError::InvalidEncoding(
                "invalid record revision width",
            ));
        }
        let (version, body) = body
            .split_at_checked(width)
            .ok_or(VersionError::InvalidEncoding("truncated record revision"))?;
        let mut full = [0; 8];
        full[8 - width..].copy_from_slice(version);
        let revision = u64::from_be_bytes(full);
        let (certificate, inline, certificate_bytes) = if layout == UNCERTIFIED {
            if !body.is_empty() {
                return Err(VersionError::InvalidEncoding(
                    "invalid uncertified record head",
                ));
            }
            (None, None, 0)
        } else if layout == INDEXED_LIVE {
            if !body.is_empty() || revision == 0 {
                return Err(VersionError::InvalidEncoding("invalid indexed live record"));
            }
            (None, None, 0)
        } else if layout == EXTERNAL {
            let certificate = body
                .try_into()
                .map_err(|_| VersionError::InvalidEncoding("invalid external record head"))?;
            (Some(certificate), None, 40)
        } else if body == [0] {
            (Some([0; 40]), Some(body), 0)
        } else if body == [1] {
            (None, Some(body), 0)
        } else {
            let (digest, encoded) =
                body.split_at_checked(32)
                    .ok_or(VersionError::InvalidEncoding(
                        "truncated inline record digest",
                    ))?;
            let Some((&1, value)) = encoded.split_first() else {
                return Err(VersionError::InvalidEncoding("invalid inline record tag"));
            };
            if value.len() > INLINE_LIMIT {
                return Err(VersionError::InvalidEncoding("oversized inline record"));
            }
            let digest = ImageDigest::from_bytes(digest.try_into().expect("fixed digest"));
            (
                Some(encode_certificate(value.len(), digest)?),
                Some(encoded),
                32,
            )
        };
        Ok(Self {
            revision,
            certificate,
            certificate_bytes,
            indexed_live: layout == INDEXED_LIVE,
            inline,
        })
    }
}

/// For a witnessed live empty record the complete image consists solely of the selected revision prefix.
pub(in crate::mvcc) fn empty_fingerprint(revision: u64) -> UnitFingerprint {
    #[cfg(test)]
    EMPTY_IMAGE_HASHES.with(|count| count.set(count.get() + 1));
    UnitFingerprint {
        decoded_bytes: 8,
        digest: RecordImageHasher::new(CommitSequence::from_u64(revision)).finish(),
    }
}

#[cfg(test)]
thread_local! {
    pub(in crate::mvcc) static EMPTY_IMAGE_HASHES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_heads_keep_full_revision_and_distinguish_empty_tombstone_and_uncertified() {
        for number in [0, 1, 255, 256, u64::from(u32::MAX) + 1, u64::MAX] {
            let revision = Revision::new(number);
            let digest = ImageDigest::of_bytes(&number.to_be_bytes());
            for live in [false, true] {
                let mut output = vec![0; revision.size(live.then_some(0)).unwrap()];
                revision.inline(&mut output, None);
                *output.last_mut().unwrap() = u8::from(live);
                let head = RecordHead::decode(&output).unwrap();
                assert_eq!(head.revision, number);
                assert_eq!(head.inline, Some(&[u8::from(live)][..]));
                assert_eq!(
                    head.history_certificate().unwrap(),
                    Some(if live {
                        encode_certificate(0, digest).unwrap()
                    } else {
                        [0; 40]
                    })
                );
                assert_eq!(head.certificate_bytes, 0);
            }
            let mut output = [0; 49];
            let unknown = RecordHead::decode(revision.external(None, &mut output)).unwrap();
            assert_eq!(unknown.revision, number);
            assert!(unknown.certificate.is_none());
            assert!(unknown.inline.is_none());
        }
        for invalid in [
            &[0x10][..],
            &[0x49],
            &[0x41, 0],
            &[0x41, 1, 0],
            &[0x81, 1],
            &[2, 1],
        ] {
            assert!(RecordHead::decode(invalid).is_err(), "{invalid:?}");
        }
    }
}
