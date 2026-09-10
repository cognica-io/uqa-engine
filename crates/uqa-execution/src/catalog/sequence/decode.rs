//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Backward-compatible SQL sequence state decoding.

use super::{
    sequence_cache_size_default, sequence_state_called_default, SequenceDataType, SequenceState,
};

impl<'de> serde::Deserialize<'de> for SequenceState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(serde::Deserialize)]
        struct Representation {
            start: i64,
            increment: i64,
            current: i64,
            #[serde(default = "sequence_state_called_default")]
            called: bool,
            #[serde(default)]
            log_count: i64,
            #[serde(default)]
            data_type: SequenceDataType,
            #[serde(default)]
            min_value: Option<i64>,
            #[serde(default)]
            max_value: Option<i64>,
            #[serde(default)]
            cycle: bool,
            #[serde(default = "sequence_cache_size_default")]
            cache_size: i64,
            #[serde(default)]
            definition_generation: [u8; 16],
            #[serde(default)]
            owner: Option<super::SequenceOwner>,
        }

        let representation = Representation::deserialize(deserializer)?;
        let (type_min, type_max) = representation.data_type.bounds();
        Ok(Self {
            start: representation.start,
            increment: representation.increment,
            current: representation.current,
            called: representation.called,
            log_count: representation.log_count,
            data_type: representation.data_type,
            min_value: representation
                .min_value
                .unwrap_or(if representation.increment > 0 {
                    1
                } else {
                    type_min
                }),
            max_value: representation
                .max_value
                .unwrap_or(if representation.increment > 0 {
                    type_max
                } else {
                    -1
                }),
            cycle: representation.cycle,
            cache_size: representation.cache_size,
            definition_generation: representation.definition_generation,
            owner: representation.owner,
        })
    }
}
