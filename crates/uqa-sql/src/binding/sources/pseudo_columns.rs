//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Query pseudo-column namespaces borrow relation identities and admit only their published virtual names.

use crate::{ColumnIdentity, ColumnType, RowSchema, SQLError};
use uqa_core::memory::{ProductionControl, ProductionMap};

pub fn with_query_table_pseudo_columns_with_control(
    schema: &RowSchema,
    control: &ProductionControl<'_>,
) -> Result<RowSchema, SQLError> {
    control.check()?;
    let mut qualifiers = ProductionMap::new(*control);
    for qualifier in schema
        .identities()
        .iter()
        .chain(
            schema
                .typed_virtual_identities()
                .map(|(identity, _)| identity),
        )
        .filter_map(ColumnIdentity::qualifier)
    {
        qualifiers.insert_copy(qualifier, ())?;
    }
    let mut selected = None;
    for (qualifier, ()) in &qualifiers {
        let mut valid = true;
        for name in ["_doc_id", "_score", "tableoid"] {
            let identity = ColumnIdentity::from_parts_with_control(Some(qualifier), name, control)?;
            if !schema.has_qualified_identity(&identity) {
                valid = false;
                break;
            }
        }
        if valid {
            if selected.is_some() {
                return Ok(schema.clone());
            }
            selected = Some(*qualifier);
        }
    }
    let Some(qualifier) = selected else {
        return Ok(schema.clone());
    };
    let names = ["_doc_id", "_score", "tableoid", "xmin"];
    let mut types = [None, None, None, None];
    for (index, name) in names.iter().enumerate() {
        let identity = ColumnIdentity::from_parts_with_control(Some(qualifier), name, control)?;
        types[index] = schema.qualified_identity_type(&identity);
    }
    let result = RowSchema::with_virtual_columns_with_control(
        schema,
        names
            .into_iter()
            .zip(types)
            .map(|(name, ty)| (None, name, ty)),
        control,
    )?;
    if result.has_qualifier(crate::semantics::META_QUALIFIER) {
        return Ok(result);
    }
    RowSchema::with_virtual_columns_with_control(
        &result,
        [
            (
                Some(crate::semantics::META_QUALIFIER),
                crate::semantics::META_DOC_ID_COLUMN,
                Some(&ColumnType::BigInteger),
            ),
            (
                Some(crate::semantics::META_QUALIFIER),
                crate::semantics::META_SCORE_COLUMN,
                Some(&ColumnType::DoublePrecision),
            ),
        ],
        control,
    )
}
