//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluated image input preserves historical policy and original admitted value owners.

use super::super::{identities::NullIdentities, values::StoredValues};
use super::{
    capture::value_error, comparison::StoredComparison, storage, value_bytes_for_payload,
    CapturedValueIndexPolicy, ColumnValueIndex, ValueIndexCache, ValueIndexComparison,
    ValueIndexImageMetadata,
};
use uqa_core::{
    image_io::{decode_image, AsyncImageInput, ImageDecoder},
    memory::{Budgeted, BudgetedVec, MemoryError, ProductionControl, RetainedMap},
    value_image::{ValueImageError, ValueImageInput},
    DocId, Value,
};
use uqa_sql::{
    catalog::table_metadata::{SharedTableMetadata, TableMetadata},
    SQLError,
};
use uqa_storage::{
    btree_index::BTreeIndexImageBuilder, read_control::StorageReadControl, StorageBackendError,
};

/// Unpublished native keys in increasing document-identity order. A rejected addition consumes the complete partial owner. Completion retains original input reservations without retaining input cancellation; the ordinary cache threshold governs later mutations.
pub struct ValueIndexImageBuilder {
    field: SharedTableMetadata<String>,
    policy: CapturedValueIndexPolicy,
    expected: usize,
    previous: Option<DocId>,
    values: RetainedMap<DocId, Budgeted<Value>>,
    index: Option<BTreeIndexImageBuilder>,
    nulls: BudgetedVec<DocId>,
    bytes: usize,
    limit: usize,
    control: StorageReadControl,
}

impl ValueIndexImageBuilder {
    pub fn new(
        metadata: ValueIndexImageMetadata,
        limit: usize,
        control: &StorageReadControl,
    ) -> Result<Self, SQLError> {
        decode_image(
            &mut std::io::empty(),
            control.memory(),
            control.cancellation(),
            async |mut input| Self::new_from(metadata, limit, control, &mut input).await,
        )
    }

    /// Decode the metadata and adopt its field policy through one retained continuation.
    pub fn image_input(
        limit: usize,
        control: &StorageReadControl,
    ) -> Result<ImageDecoder<Self, SQLError>, SQLError> {
        let retained = control.clone();
        Ok(ImageDecoder::new(
            control.memory(),
            control.cancellation(),
            |mut reader| async move {
                let mut input = reader.input();
                Self::read_from(limit, &retained, &mut input).await
            },
        )?)
    }

    pub(super) async fn read_from(
        limit: usize,
        control: &StorageReadControl,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<Self, SQLError> {
        let metadata = ValueIndexImageMetadata::read_image_from(input)
            .await
            .map_err(storage)?;
        Self::new_from(metadata, limit, control, input).await
    }

    pub async fn new_from(
        metadata: ValueIndexImageMetadata,
        limit: usize,
        control: &StorageReadControl,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<Self, SQLError> {
        input
            .with_scope(
                control.memory(),
                control.cancellation(),
                async |mut child| {
                    Self::adopt_metadata_from(metadata, limit, control, &mut child).await
                },
            )
            .await
    }

    async fn adopt_metadata_from(
        metadata: ValueIndexImageMetadata,
        limit: usize,
        control: &StorageReadControl,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<Self, SQLError> {
        control.check().map_err(storage)?;
        let (field, memory) = metadata.field.into_parts();
        let field = Budgeted::new(field, memory);
        if !field.budget().shares_allowance(control.memory())
            || field.reserved_bytes() < field.capacity()
        {
            return Err(invalid(
                "evaluated index field belongs to another input allowance",
            ));
        }
        let index = if metadata.policy.carried {
            None
        } else {
            let field = input.copy_text(&field).await?;
            Some(BTreeIndexImageBuilder::new(field, control).map_err(storage)?)
        };
        let field = TableMetadata::from_admitted(field, control.cancellation())?.snapshot();
        Ok(Self {
            field,
            policy: metadata.policy,
            expected: metadata.len,
            previous: None,
            values: RetainedMap::new(control.memory()),
            index,
            nulls: BudgetedVec::new(control.memory()),
            bytes: 0,
            limit,
            control: control.clone(),
        })
    }

    pub fn add_value(self, id: DocId, value: Budgeted<Value>) -> Result<Self, SQLError> {
        let control = self.control.clone();
        decode_image(
            &mut std::io::empty(),
            control.memory(),
            control.cancellation(),
            async |mut input| self.add_value_from(id, value, &mut input).await,
        )
    }

    /// Decode and adopt one exact evaluated value without replaying its expression or losing the original input owner.
    pub fn value_input(self, id: DocId) -> Result<ImageDecoder<Self, SQLError>, SQLError> {
        let control = self.control.clone();
        Ok(ImageDecoder::new(
            control.memory(),
            control.cancellation(),
            |mut reader| async move {
                let mut input = reader.input();
                self.read_value_from(id, &mut input).await
            },
        )?)
    }

    pub(super) async fn read_value_from(
        self,
        id: DocId,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<Self, SQLError> {
        let value = ValueImageInput::read_from(input)
            .await
            .map_err(input_value_error)?;
        input.end().await?;
        self.add_value_from(id, value, input).await
    }

    pub(super) fn is_complete(&self) -> bool {
        self.values.len() == self.expected
    }

    pub async fn add_value_from(
        self,
        id: DocId,
        value: Budgeted<Value>,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<Self, SQLError> {
        let control = self.control.clone();
        input
            .with_scope(
                control.memory(),
                control.cancellation(),
                async |mut child| self.adopt_value_from(id, value, &mut child).await,
            )
            .await
    }

    async fn adopt_value_from(
        mut self,
        id: DocId,
        value: Budgeted<Value>,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<Self, SQLError> {
        self.control.check().map_err(storage)?;
        if self.values.len() == self.expected
            || self.previous.is_some_and(|previous| previous >= id)
        {
            return Err(invalid(
                "evaluated image has excess, duplicate or unordered identities",
            ));
        }
        if !value.budget().shares_allowance(self.control.memory()) {
            return Err(invalid(
                "evaluated image value belongs to another input allowance",
            ));
        }
        let production = ProductionControl::new(
            self.control.memory(),
            self.control.cancellation(),
            input.cancellation,
        );
        let mut payload = 0usize;
        for bytes in value.retained_payload_parts_with_control(production) {
            input.checkpoint().await?;
            payload = payload
                .checked_add(bytes.map_err(|error| storage(value_error(error)))?)
                .ok_or_else(|| storage(MemoryError::SizeOverflow.into()))?;
        }
        if value.reserved_bytes() < payload {
            return Err(invalid(
                "evaluated image value has no complete input admission",
            ));
        }
        validate_policy(self.policy, &value, input).await?;
        self.bytes = self
            .bytes
            .checked_add(value_bytes_for_payload(payload)?)
            .ok_or_else(|| storage(MemoryError::SizeOverflow.into()))?;
        if let Some(index) = self.index.take() {
            let index = if matches!(*value, Value::Null) {
                self.nulls.push(id).map_err(|error| storage(error.into()))?;
                index
            } else {
                let key = value
                    .clone_budgeted_from(input)
                    .await
                    .map_err(|error| storage(value_error(error)))?;
                index
                    .add_value_from(id, key, input)
                    .await
                    .map_err(storage)?
            };
            self.index = Some(index);
        }
        self.values
            .insert(id, value, payload)
            .map_err(|error| storage(error.into()))?;
        self.previous = Some(id);
        self.control.check().map_err(storage)?;
        Ok(self)
    }

    pub fn finish(self) -> Result<ValueIndexCache, SQLError> {
        self.control.check().map_err(storage)?;
        if self.values.len() != self.expected {
            return Err(invalid("evaluated image is missing declared values"));
        }
        let (nulls, memory) = self.nulls.into_parts();
        let nulls = Budgeted::new(nulls, memory)
            .into_shared()
            .map_err(|error| storage(error.into()))?;
        Ok(ValueIndexCache {
            policy: ColumnValueIndex {
                index: self
                    .index
                    .map(BTreeIndexImageBuilder::finish)
                    .transpose()
                    .map_err(storage)?,
                values: StoredValues::from_admitted(self.values),
                nulls: NullIdentities::Admitted(nulls),
                has_temporal: self.policy.has_temporal,
                has_row_values: self.policy.has_row_values,
                comparison: match self.policy.comparison {
                    ValueIndexComparison::Ordered => StoredComparison::Ordered,
                    ValueIndexComparison::TypedField => StoredComparison::TypedField,
                    ValueIndexComparison::Fallible => StoredComparison::Fallible,
                },
            },
            retained: None,
            field: self.field,
            limit: self.limit,
            resident_bytes: self.bytes,
        })
    }
}

async fn validate_policy(
    policy: CapturedValueIndexPolicy,
    value: &Value,
    input: &mut AsyncImageInput<'_>,
) -> Result<(), SQLError> {
    // Carried values have never populated physical key eligibility. Preserve their stored policy without inferring comparison requirements from those values.
    if policy.carried {
        return Ok(());
    }
    let temporal = super::super::value_is_temporal(value);
    let row = super::super::value_is_row(value);
    let fallible = uqa_sql::expr::value_comparison_can_fail_from(value, input).await?;
    let typed = super::comparison::field_needs_typed_comparison_from(value, input).await?;
    if (temporal && !policy.has_temporal)
        || (row && !policy.has_row_values)
        || (fallible && policy.comparison != ValueIndexComparison::Fallible)
        || (typed && policy.comparison == ValueIndexComparison::Ordered)
    {
        return Err(invalid(
            "evaluated image policy contradicts its stored values",
        ));
    }
    Ok(())
}

fn invalid(message: &'static str) -> SQLError {
    storage(StorageBackendError::Other(message.into()))
}

fn input_value_error(error: ValueImageError) -> SQLError {
    storage(match error {
        ValueImageError::Memory(error) => StorageBackendError::Memory(error),
        ValueImageError::Cancelled(error) => StorageBackendError::Cancelled(error),
        error => StorageBackendError::backend("evaluated-index value image", error),
    })
}

#[cfg(test)]
mod tests;
