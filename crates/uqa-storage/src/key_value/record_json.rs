//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Controlled JSON record buffers shared by vector persistence codecs.

use crate::{
    mvcc::{VersionError, VersionResult},
    read_control::StorageReadControl,
};
use serde::{
    de::{SeqAccess, Visitor},
    Deserialize, Deserializer, Serialize,
};
use uqa_core::memory::BudgetedVec;

pub(super) fn encode(
    value: &impl Serialize,
    control: &StorageReadControl,
) -> VersionResult<BudgetedVec<u8>> {
    let mut writer = Writer {
        output: BudgetedVec::new(control.memory()),
        control,
        failure: None,
    };
    let result = serde_json::to_writer(&mut writer, value);
    if let Some(error) = writer.failure {
        return Err(error);
    }
    result.map_err(crate::StorageBackendError::from)?;
    Ok(writer.output)
}

/// Elements are scalars or borrowed raw JSON; nested owned arrays are decoded separately under the same allowance.
pub(super) fn array<'de, T: Deserialize<'de>>(
    json: &'de str,
    control: &StorageReadControl,
) -> VersionResult<BudgetedVec<T>> {
    struct Elements<'a, T> {
        values: BudgetedVec<T>,
        control: &'a StorageReadControl,
        failure: &'a mut Option<VersionError>,
    }
    impl<'de, T: Deserialize<'de>> Visitor<'de> for Elements<'_, T> {
        type Value = BudgetedVec<T>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a JSON array")
        }
        fn visit_seq<A: SeqAccess<'de>>(
            mut self,
            mut sequence: A,
        ) -> Result<Self::Value, A::Error> {
            loop {
                if let Err(error) = self.control.cancellation().check() {
                    *self.failure = Some(error.into());
                    return Err(serde::de::Error::custom("vector decoding interrupted"));
                }
                let Some(value) = sequence.next_element()? else {
                    return Ok(self.values);
                };
                // Scalars and borrowed elements allocate no payload before the controlled append.
                if let Err(error) = self.values.push(value) {
                    *self.failure = Some(error.into());
                    return Err(serde::de::Error::custom("vector decoding interrupted"));
                }
            }
        }
    }
    let mut failure = None;
    let mut decoder = serde_json::Deserializer::from_str(json);
    let result = decoder.deserialize_seq(Elements {
        values: BudgetedVec::new(control.memory()),
        control,
        failure: &mut failure,
    });
    if let Some(error) = failure {
        return Err(error);
    }
    let values = result.map_err(crate::StorageBackendError::from)?;
    decoder.end().map_err(crate::StorageBackendError::from)?;
    Ok(values)
}

struct Writer<'a> {
    output: BudgetedVec<u8>,
    control: &'a StorageReadControl,
    failure: Option<VersionError>,
}
impl std::io::Write for Writer<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let result = self
            .control
            .cancellation()
            .check()
            .map_err(VersionError::from)
            .and_then(|()| {
                self.output
                    .extend_from_slice(bytes)
                    .map_err(VersionError::from)
            });
        if let Err(error) = result {
            self.failure = Some(error);
            return Err(std::io::Error::other("vector encoding interrupted"));
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
