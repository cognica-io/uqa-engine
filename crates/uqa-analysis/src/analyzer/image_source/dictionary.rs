//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Encoded resource owners never retain imported admission or decoded dictionary models.

use std::sync::Arc;
use uqa_core::memory::Produced;

#[derive(Debug)]
enum Bytes {
    Static(&'static [u8]),
    Shared(Arc<[u8]>),
    Owned(Vec<u8>),
}

#[derive(Debug)]
struct DictionaryData {
    bytes: Bytes,
    retained_bytes: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct DictionaryImage(Arc<Produced<DictionaryData>>);

impl DictionaryImage {
    pub(super) fn bytes(&self) -> &[u8] {
        match &self.0.bytes {
            Bytes::Static(bytes) => bytes,
            Bytes::Shared(bytes) => bytes,
            Bytes::Owned(bytes) => bytes,
        }
    }

    pub(super) fn retained_bytes(&self) -> usize {
        self.0.retained_bytes
    }
}

impl DictionaryImage {
    pub(crate) async fn prepare_with(
        source: &crate::morphology::resources::DictionaryBytes,
        control: &uqa_core::memory::ProductionControl<'_>,
        work: &mut impl crate::work::Work<uqa_core::ValueRetentionError>,
    ) -> Result<Self, uqa_core::ValueRetentionError> {
        use crate::morphology::resources::DictionaryBytes;
        use uqa_core::memory::{MemoryError, ProductionVec};
        control.check()?;
        let (bytes, memory, payload_bytes) = match source {
            DictionaryBytes::Static(bytes) => (
                Bytes::Static(bytes),
                control.empty_reservation(),
                bytes.len(),
            ),
            DictionaryBytes::Shared(bytes) => (
                Bytes::Shared(bytes.clone()),
                control.empty_reservation(),
                bytes.len(),
            ),
            DictionaryBytes::Retained(source) => {
                let mut bytes = ProductionVec::new(*control);
                bytes.reserve(source.len())?;
                for part in source.chunks(4096) {
                    work.checkpoint().await?;
                    bytes.extend_from_slice(part)?;
                }
                let (bytes, memory) = bytes.finish()?.into_parts();
                let payload_bytes = bytes.capacity();
                (Bytes::Owned(bytes), memory, payload_bytes)
            }
        };
        let retained_bytes = size_of::<Produced<DictionaryData>>()
            .checked_add(payload_bytes)
            .ok_or(MemoryError::SizeOverflow)?;
        let source = control
            .finish(
                DictionaryData {
                    bytes,
                    retained_bytes,
                },
                memory,
            )?
            .into_shared(control)?;
        Ok(Self(source))
    }
}

#[cfg(any(feature = "nori", feature = "kuromoji"))]
mod preparation {
    use super::{Arc, DictionaryData, DictionaryImage, Produced};
    use crate::morphology::resources::DictionaryBytes;
    use parking_lot::Mutex;
    use std::sync::Weak;
    use uqa_core::{memory::ProductionControl, ValueRetentionError};

    #[derive(Debug, Default)]
    pub(crate) struct DictionaryImageCache(Mutex<Weak<Produced<DictionaryData>>>);

    impl DictionaryImageCache {
        pub(crate) fn prepare(
            &self,
            source: &DictionaryBytes,
            control: &ProductionControl<'_>,
        ) -> Result<DictionaryImage, ValueRetentionError> {
            control.check()?;
            let mut cached = self.0.lock();
            control.check()?;
            if let Some(source) = cached.upgrade() {
                return Ok(DictionaryImage(source));
            }
            let source =
                crate::work::inline(DictionaryImage::prepare_with(source, control, &mut ()))?;
            *cached = Arc::downgrade(&source.0);
            Ok(source)
        }
    }
}

#[cfg(any(feature = "nori", feature = "kuromoji"))]
pub(crate) use preparation::DictionaryImageCache;
