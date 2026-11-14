//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete physical OID input precedes ordered decimal and object-identity validation.

use super::{
    copy_relation, sql_error, AsyncImageInput, BudgetedVec, CatalogFacade, CatalogInputLimits,
    Rows, SequenceCatalog, SequenceRecord, StorageBackendError, StorageBackendResult,
    StorageReadControl,
};
use crate::catalog::sequence::catalog_oids::{
    decode_object_id, SEQUENCE_CATALOG_OID_METADATA_PREFIX,
};
use uqa_storage::catalog::text_input::CatalogTextStep;

pub(super) async fn read(
    result: &mut SequenceCatalog,
    catalog: &dyn CatalogFacade,
    limits: CatalogInputLimits,
    control: &StorageReadControl,
    input: &mut AsyncImageInput<'_>,
) -> StorageBackendResult<()> {
    input.checkpoint().await?;
    let mut source =
        catalog.begin_metadata_input(SEQUENCE_CATALOG_OID_METADATA_PREFIX, limits, control)?;
    let mut decoded = BudgetedVec::new(control.memory());
    loop {
        input.checkpoint().await?;
        match source.advance()? {
            CatalogTextStep::Pending => (),
            CatalogTextStep::Row(row) => decoded.push(row)?,
            CatalogTextStep::Complete => break,
        }
    }
    drop(source);
    let mut rows = Rows::from(decoded);
    for row in rows.entries.by_ref() {
        input.checkpoint().await?;
        let oid = decimal(&row.value, input).await?.ok_or_else(|| {
            StorageBackendError::Other(format!(
                "sequence catalog OID entry `{}` holds invalid OID `{}`",
                row.name, row.value
            ))
        })?;
        let encoded = row
            .name
            .strip_prefix(SEQUENCE_CATALOG_OID_METADATA_PREFIX)
            .unwrap_or(&row.name);
        let object_id = decode_object_id(encoded)?;
        result.catalog_oids.insert(object_id, oid, 0)?;
    }
    drop(rows);
    // The source can contain superseded names. Rebuild the identity membership from the resulting native records after the complete ordered duplicate-identity check.
    let previous = result.records.clone();
    let mut objects = uqa_core::memory::RetainedMap::new(control.memory());
    for (relation, record) in previous.iter() {
        input.checkpoint().await?;
        objects.insert(record.object_id, record.state.definition_generation, 0)?;
        let oid = result.catalog_oids.get(&record.object_id).copied();
        if record.catalog_oid != oid {
            let relation = copy_relation(relation, input).await?;
            result
                .records
                .insert_image_entry(
                    relation,
                    SequenceRecord {
                        catalog_oid: oid,
                        ..*record
                    },
                )
                .map_err(sql_error)?;
        }
    }
    result.durable_objects = objects;
    Ok(())
}

async fn decimal(
    value: &str,
    input: &mut AsyncImageInput<'_>,
) -> StorageBackendResult<Option<u32>> {
    let digits = value.strip_prefix('+').unwrap_or(value).as_bytes();
    if digits.is_empty() {
        return Ok(None);
    }
    let mut result = 0u32;
    for chunk in digits.chunks(256) {
        input.checkpoint().await?;
        for digit in chunk {
            if !digit.is_ascii_digit() {
                return Ok(None);
            }
            let Some(next) = result
                .checked_mul(10)
                .and_then(|value| value.checked_add(u32::from(digit - b'0')))
            else {
                return Ok(None);
            };
            result = next;
        }
    }
    Ok(Some(result))
}
