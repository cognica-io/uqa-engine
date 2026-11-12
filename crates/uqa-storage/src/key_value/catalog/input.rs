//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained definition input uses the catalog's existing committed/private boundary.

use super::{KeyValueCatalog, TAG_CATALOG_INDEX};
use crate::{
    catalog::input::{strings, CatalogInput, CatalogInputLimits, CatalogRowDecoder},
    key_value::codec::read_str_ref,
    read_control::StorageReadControl,
    CatalogIndexRow, RelationIdentity, StorageBackendError, StorageBackendResult,
};
use uqa_core::{
    memory::{Budgeted, ProductionControl},
    RelationName, RelationReference,
};

impl KeyValueCatalog {
    pub(super) fn schema_load(
        &self,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<crate::catalog::schema::load::CatalogSchemaLoad> {
        use crate::catalog::schema::load::{CatalogSchemaLoad, CatalogSchemaOrder};
        CatalogSchemaLoad::new(
            self.row_input(&[super::TAG_SCHEMA], decode_schema, limits, control)?,
            CatalogSchemaOrder::Name,
            control,
        )
    }

    pub(super) fn row_input<T>(
        &self,
        prefix: &[u8],
        decode: CatalogRowDecoder<T>,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<CatalogInput<T>> {
        CatalogInput::new(
            self.retained_input_view(prefix, control)?,
            prefix,
            decode,
            limits,
            control,
        )
    }

    pub(super) fn graph_name_input(
        &self,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<CatalogInput<String>> {
        let prefix = &[super::TAG_NAMED_GRAPH];
        CatalogInput::new_keys(
            self.retained_input_view(prefix, control)?,
            prefix,
            decode_graph_name,
            limits,
            control,
        )
    }

    fn retained_input_view(
        &self,
        prefix: &[u8],
        control: &StorageReadControl,
    ) -> StorageBackendResult<std::sync::Arc<dyn crate::key_value::KeyValueRead + Send + Sync>>
    {
        control.check()?;
        if !self.store.transaction_model().is_versioned() {
            return Err(StorageBackendError::Other(
                "retained catalog input requires a versioned read owner".into(),
            ));
        }
        crate::key_value::index_view::read_view(self.store.as_ref(), |read| read.retain(&[prefix]))
    }

    pub(super) fn index_input(
        &self,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<CatalogInput<CatalogIndexRow>> {
        self.row_input(&[TAG_CATALOG_INDEX], decode_index, limits, control)
    }
}

fn decode_schema(
    key: &[u8],
    value: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<Option<Budgeted<crate::SchemaRow>>> {
    let mut offset = 1;
    let name = read_str_ref(key, &mut offset)?;
    let row = if value == name.as_bytes() {
        uqa_core::catalog_schema::SchemaRow::legacy_with_control(
            name,
            &ProductionControl::new(
                control.memory(),
                control.cancellation(),
                control.cancellation(),
            ),
        )?
        .into_budgeted()
        .expect("admitted legacy schema")
        .try_map(|row| Ok::<_, StorageBackendError>(crate::SchemaRow::Legacy(row)))?
    } else {
        crate::catalog::schema::input::row(value, control)?
    };
    if row.name() != name {
        return Err(StorageBackendError::Other(format!(
            "schema catalog key `{name}` disagrees with stored name `{}`",
            row.name()
        )));
    }
    Ok(Some(row))
}

fn decode_index(
    key: &[u8],
    value: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<Option<Budgeted<CatalogIndexRow>>> {
    let relation = relation(key, control)?;
    let [index_type, table_name, columns, parameters, definition] = strings::fields(
        value,
        [
            ("index_type", false),
            ("table_name", false),
            ("columns_json", false),
            ("parameters_json", false),
            ("definition_json", true),
        ],
        control,
    )?;
    Ok(Some(crate::catalog::input::index::row(
        relation,
        [index_type, table_name, columns, parameters].map(|value| value.expect("required field")),
        definition,
    )))
}

fn relation(
    key: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<RelationIdentity>> {
    let mut offset = 1;
    let first = read_str_ref(key, &mut offset)?;
    let reference = if offset == key.len() {
        RelationIdentity::try_borrowed_reference(first)
            .map_err(|error| StorageBackendError::Other(error.to_string()))?
    } else {
        let name = read_str_ref(key, &mut offset)?;
        if offset != key.len() {
            return Err(StorageBackendError::Other(
                "corrupt relation catalog key has trailing bytes".into(),
            ));
        }
        RelationReference {
            schema: Some(RelationName::decoded(first)),
            name: RelationName::decoded(name),
        }
    };
    Ok(reference
        .identity_with_control(
            RelationName::decoded("public"),
            &ProductionControl::new(
                control.memory(),
                control.cancellation(),
                control.cancellation(),
            ),
        )?
        .into_budgeted()
        .expect("catalog input retains an allowance"))
}

#[cfg(test)]
mod tests;

fn decode_graph_name(
    key: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<Option<Budgeted<String>>> {
    let name = read_str_ref(key, &mut 1)?;
    Ok(Some(
        ProductionControl::new(
            control.memory(),
            control.cancellation(),
            control.cancellation(),
        )
        .copy_text(name)?
        .into_budgeted()
        .expect("retained graph name"),
    ))
}
