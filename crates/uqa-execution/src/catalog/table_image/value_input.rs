//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A selected evaluated-index component retains its address until native registration completes.

use super::{invalid, sql_error, MemoryValueIndexesBuilder};
use std::io::Read;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    DocId,
};
use uqa_sql::SQLError;
use uqa_storage::{
    backup::{DecodedMemoryTableUnit, MemoryImageName},
    StorageBackendResult,
};

pub(super) enum ValueInput {
    Ready(MemoryValueIndexesBuilder),
    Reading {
        name: MemoryImageName,
        document: Option<DocId>,
        input: ImageDecoder<MemoryValueIndexesBuilder, SQLError>,
    },
}

impl ValueInput {
    pub(super) fn reading(&self) -> bool {
        matches!(self, Self::Reading { .. })
    }

    pub(super) fn advance(
        self,
        unit: &DecodedMemoryTableUnit,
        reader: &mut dyn Read,
        window: ImageDecodeWindow,
        limit: usize,
    ) -> StorageBackendResult<(Self, bool)> {
        let DecodedMemoryTableUnit::ValueIndex { name, document } = unit else {
            return Err(invalid("evaluated index input has not completed"));
        };
        let mut input = match self {
            Self::Ready(indexes) => match document {
                None => indexes
                    .image_input(name.clone(), limit)
                    .map_err(sql_error)?,
                Some(id) => indexes.value_input(name, *id).map_err(sql_error)?,
            },
            Self::Reading {
                name: selected,
                document: selected_document,
                input,
            } if name == &selected && *document == selected_document => input,
            Self::Reading { .. } => {
                return Err(invalid(
                    "evaluated index input changed its selected address",
                ));
            }
        };
        Ok(match input.advance(reader, window).map_err(sql_error)? {
            Some(indexes) => (Self::Ready(indexes), true),
            None => (
                Self::Reading {
                    name: name.clone(),
                    document: *document,
                    input,
                },
                false,
            ),
        })
    }

    pub(super) fn finish(self) -> StorageBackendResult<super::ValueIndexes> {
        match self {
            Self::Ready(indexes) => indexes.finish(),
            Self::Reading { .. } => Err(invalid("evaluated index input has not completed")),
        }
    }
}
