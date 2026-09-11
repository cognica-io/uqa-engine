//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog-backed identity formatting for removal diagnostics.

use super::{analysis_relations, BTreeSet, RoutineDropTarget, RoutineRemovalContext, SQLError};

impl analysis_relations::RoutineRelationOids for crate::catalog::context::CatalogContext<'_> {
    fn bound_regclass_oid(&self, name: &str) -> Result<Option<i64>, SQLError> {
        crate::catalog::projection::resolve_bound_regclass_oid(self, name)
    }
}

pub fn routine_drop_display_label(
    context: &RoutineRemovalContext<'_>,
    target: &RoutineDropTarget,
) -> Result<String, SQLError> {
    let label = target.label();
    let oid = crate::catalog::projection::resolve_regprocedure_oid(&context.catalog, &label)
        .map_err(SQLError::Internal)?
        .ok_or_else(|| {
            SQLError::Internal(format!("resolved routine {label} has no catalog OID"))
        })?;
    crate::catalog::projection::resolve_regtype_output(
        &context.catalog,
        &uqa_sql::ast::ColumnType::Regprocedure,
        oid,
    )
    .map_err(SQLError::Internal)?
    .ok_or_else(|| SQLError::Internal(format!("resolved routine {label} has no display identity")))
}

pub fn relation_dependents_drop_error(
    context: &RoutineRemovalContext<'_>,
    names: &[String],
    kind: &str,
) -> Result<SQLError, SQLError> {
    let names = names.iter().collect::<BTreeSet<_>>();
    let message = if names.len() == 1 {
        let name = *names.first().expect("one root relation");
        let label = match crate::catalog::projection::resolve_regclass_oid(&context.catalog, name)?
        {
            Some(oid) => crate::catalog::projection::resolve_regtype_output(
                &context.catalog,
                &uqa_sql::ast::ColumnType::Regclass,
                oid,
            )
            .map_err(SQLError::Internal)?
            .unwrap_or_else(|| name.clone()),
            None => name.clone(),
        };
        format!("cannot drop {kind} {label} because other objects depend on it")
    } else {
        "cannot drop desired object(s) because other objects depend on them".into()
    };
    Ok(SQLError::Routine {
        sqlstate: "2BP01".into(),
        message,
    })
}
