//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored fields, public aliases and executor-only metadata share one admitted positional layout.

use crate::RowSchema;
use uqa_core::memory::{ProductionControl, ProductionVec};
use uqa_sql::{
    ast::{ColumnDef, ColumnType, CompositeTypeReference, InternalRelationId},
    plan::source_projection::RelationMetadataProjection,
    semantics::{TABLE_OID_COLUMN, XMIN_COLUMN},
    SQLError,
};

pub(super) fn projected_scan_schema_with_control(
    qualifier: &str,
    columns: &[String],
    visible: &[String],
    definitions: &[ColumnDef],
    include_table_oid: bool,
    row_type: Option<&CompositeTypeReference>,
    control: &ProductionControl<'_>,
) -> Result<RowSchema, SQLError> {
    let metadata = columns.iter().zip(visible).map(|(column, visible)| {
        let definition = definitions
            .iter()
            .find(|definition| definition.name == *column);
        (
            visible.as_str(),
            definition
                .map(|definition| &definition.ty)
                .or_else(|| (column == XMIN_COLUMN).then_some(&ColumnType::Xid)),
            definition.and_then(|definition| definition.collation_oid),
        )
    });
    let oid = include_table_oid.then_some((TABLE_OID_COLUMN, Some(&ColumnType::Oid), None));
    let mut schema = RowSchema::with_column_metadata_with_control(
        Some(qualifier),
        metadata.chain(oid),
        control,
    )?;
    let mut hidden = ProductionVec::new(*control);
    for (position, name) in visible.iter().enumerate() {
        if name == XMIN_COLUMN || name == TABLE_OID_COLUMN {
            hidden.push_copy(position)?;
        }
    }
    if !hidden.is_empty() {
        schema = RowSchema::with_wildcard_hidden_positions_with_control(
            &schema,
            hidden.iter().copied(),
            control,
        )?;
    }
    if let Some(row_type) = row_type {
        let reference = row_type.clone_with_control(control)?;
        let (reference, _memory) = reference.into_parts();
        schema = schema.with_relation_row_type_with_control(
            qualifier,
            &ColumnType::Composite(reference),
            control,
        )?;
    }
    Ok(schema)
}

pub(super) fn append_scan_metadata(
    input: &RowSchema,
    qualifier: &str,
    metadata: RelationMetadataProjection,
    control: &ProductionControl<'_>,
) -> Result<RowSchema, SQLError> {
    use uqa_sql::semantics::{
        DOC_ID_COLUMN, META_DOC_ID_COLUMN, META_QUALIFIER, META_SCORE_COLUMN, SCORE_COLUMN,
    };
    let relation = InternalRelationId::allocate();
    let attributes = [
        metadata
            .includes_doc_id()
            .then_some((relation.column(0), Some(&ColumnType::BigInteger))),
        metadata
            .includes_score()
            .then_some((relation.column(1), Some(&ColumnType::DoublePrecision))),
    ];
    if attributes.iter().all(Option::is_none) {
        control.check()?;
        return Ok(input.clone());
    }
    let schema = RowSchema::append_internal_typed_with_control(
        input,
        attributes.into_iter().flatten(),
        control,
    )?;
    let mut aliases = ProductionVec::new(*control);
    for (included, column, internal_name, visible, ty) in [
        (
            metadata.includes_doc_id(),
            relation.column(0),
            META_DOC_ID_COLUMN,
            DOC_ID_COLUMN,
            &ColumnType::BigInteger,
        ),
        (
            metadata.includes_score(),
            relation.column(1),
            META_SCORE_COLUMN,
            SCORE_COLUMN,
            &ColumnType::DoublePrecision,
        ),
    ] {
        if !included {
            continue;
        }
        let slot = schema
            .internal_slot(column)
            .expect("scan metadata has a physical slot");
        aliases.push_copy((Some(META_QUALIFIER), internal_name, slot, Some(ty)))?;
        if !schema.has_qualified_column(qualifier, visible) {
            aliases.push_copy((Some(qualifier), visible, slot, Some(ty)))?;
        }
        if !schema.has_unqualified_column(visible) {
            aliases.push_copy((None, visible, slot, Some(ty)))?;
        }
    }
    RowSchema::with_physical_identity_aliases_with_control(
        &schema,
        aliases.iter().copied(),
        control,
    )
}
