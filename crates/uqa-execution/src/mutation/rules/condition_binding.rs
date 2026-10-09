//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind rule inputs as expressions so conditional evaluation controls projection.

use super::{
    BTreeMap, RuleColumnMetadata, RuleContext, RuleRowImage, RuleRowSide, SQLError, Value,
};
use uqa_sql::{catalog::roles::RoleReference, ScalarExpr};

pub(super) fn rule_condition_matches<F>(
    context: RuleContext<'_>,
    rule: &uqa_sql::catalog::events::StoredRule,
    privilege_subject: &RoleReference,
    row_index: usize,
    row: &mut RuleRowImage,
    columns: &BTreeMap<String, RuleColumnMetadata>,
    project: &mut F,
) -> Result<bool, SQLError>
where
    F: FnMut(
        usize,
        RuleRowSide,
        &std::collections::BTreeSet<String>,
    ) -> Result<super::RuleInputProjection, SQLError>,
{
    if rule.definition.condition.is_none() {
        return Ok(true);
    }
    let (plan, binding) = rule
        .bound_condition_plan()
        .ok_or_else(|| SQLError::Internal("stored rule condition has no analyzed plan".into()))?;
    let mut plan = plan.clone();
    let required = binding.referenced_columns(&plan);
    let mut inputs = BTreeMap::new();
    let mut source = super::RuleInputProjection::default().source;
    for (side, relation) in [
        (RuleRowSide::Old, binding.old_relation()),
        (RuleRowSide::New, binding.new_relation()),
    ] {
        let record = match side {
            RuleRowSide::Old => &row.old,
            RuleRowSide::New => &row.new,
        };
        let Some(record) = record else {
            continue;
        };
        let missing = required
            .iter()
            .filter(|column| Some(column.relation()) == relation)
            .filter_map(|column| binding.column_name(*column))
            .filter(|name| {
                !record.contains_key(*name)
                    && columns
                        .get(*name)
                        .is_some_and(|column| !column.uses_document_id)
            })
            .map(str::to_string)
            .collect::<std::collections::BTreeSet<_>>();
        if missing.is_empty() {
            continue;
        }
        let projected = project(row_index, side, &missing)?;
        super::inputs::append_source(&mut source, &projected.source);
        for (name, input) in projected.expressions {
            let column = match side {
                RuleRowSide::Old => binding.old_column(&name),
                RuleRowSide::New => binding.new_column(&name),
            }
            .ok_or_else(|| SQLError::UnknownColumn(name.clone()))?;
            if let ScalarExpr::InternalColumn(input_column) = &input.scalar {
                use uqa_sql::expr::RowLookup;
                if let Some(value) = projected.source.view().internal_column(*input_column) {
                    let record = match side {
                        RuleRowSide::Old => &mut row.old,
                        RuleRowSide::New => &mut row.new,
                    };
                    if let Some(record) = record {
                        record.insert(name, value.clone());
                    }
                }
            }
            inputs.insert(column, input);
        }
    }
    for column in required {
        if inputs.contains_key(&column) {
            continue;
        }
        let name = binding.column_name(column).expect("collected rule input");
        let metadata = columns
            .get(name)
            .ok_or_else(|| SQLError::UnknownColumn(name.into()))?;
        let (record, doc_id) = if Some(column.relation()) == binding.old_relation() {
            (row.old.as_ref(), row.old_doc_id)
        } else {
            (row.new.as_ref(), row.new_doc_id)
        };
        let value = input_value(record, doc_id, name, metadata)?;
        let mut projected = super::RuleInputProjection::values([(
            name.to_string(),
            value,
            Some(metadata.ty.clone()),
        )]);
        super::inputs::append_source(&mut source, &projected.source);
        inputs.insert(
            column,
            projected.expressions.remove(name).expect("single input"),
        );
    }
    uqa_sql::plan::input_projection::substitute_expression_inputs(&mut plan, &inputs)?;
    Ok(uqa_sql::expr::truthy(
        &context.expressions.evaluate_stored(
            &plan,
            &source.schema,
            &source.row,
            privilege_subject,
        )?,
    ))
}

fn input_value(
    record: Option<&uqa_storage::document_store::Document>,
    doc_id: Option<uqa_core::DocId>,
    name: &str,
    metadata: &RuleColumnMetadata,
) -> Result<Value, SQLError> {
    if let Some(value) = record.and_then(|record| record.get(name)) {
        return Ok(value.clone());
    }
    if metadata.uses_document_id {
        return doc_id
            .filter(|id| uqa_sql::semantics::key_identity::is_key_document_id(*id))
            .map(i64::try_from)
            .transpose()
            .map_err(|_| SQLError::TypeMismatch("document id exceeds PostgreSQL bigint".into()))
            .map(|value| value.map_or(Value::Null, Value::Int));
    }
    Ok(Value::Null)
}
