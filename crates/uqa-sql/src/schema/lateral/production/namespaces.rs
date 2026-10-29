//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    boxed_text, inline, Arc, DiagnosticNamespace, ProductionControl, ProductionMap, ProductionVec,
    RowSchema, SQLError, SchemaMemory,
};

pub(super) fn append(
    schema: &RowSchema,
    output: &mut ProductionVec<'_, Arc<DiagnosticNamespace>>,
    unique: bool,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    if let Some(scope) = &schema.index.cold.lateral_restrictions {
        if !scope.column_namespaces.is_empty() {
            for namespace in &scope.column_namespaces {
                control.check()?;
                if !unique || !output.iter().any(|current| Arc::ptr_eq(current, namespace)) {
                    output.push_produced(inline(namespace.clone(), control)?)?;
                }
            }
            return Ok(());
        }
    }
    let mut seen = ProductionMap::new(*control);
    for name in schema
        .identities()
        .iter()
        .filter_map(crate::ColumnIdentity::qualifier)
        .chain(schema.relation_qualifiers())
    {
        control.check()?;
        if seen.insert_copy(name, ())?.is_some() {
            continue;
        }
        let mut columns = ProductionVec::new(*control);
        schema.visit_qualified_star_positions(name, control, |name, _, _, _| {
            columns.push_produced(boxed_text(name, control)?)?;
            Ok(())
        })?;
        output.push_produced(inline(produce(name, columns, control)?, control)?)?;
    }
    Ok(())
}

pub(super) fn produce(
    name: &str,
    columns: ProductionVec<'_, Box<str>>,
    control: &ProductionControl<'_>,
) -> Result<Arc<DiagnosticNamespace>, SQLError> {
    let name = boxed_text(name, control)?;
    let columns = columns.finish()?;
    let header = control.reserve(size_of::<DiagnosticNamespace>())?;
    let (name, name_memory) = name.into_parts();
    let (columns, columns_memory) = columns.into_parts();
    Ok(Arc::new(DiagnosticNamespace {
        name,
        columns,
        _memory: SchemaMemory::new(
            control.combine(header, control.combine(name_memory, columns_memory)),
        ),
    }))
}
