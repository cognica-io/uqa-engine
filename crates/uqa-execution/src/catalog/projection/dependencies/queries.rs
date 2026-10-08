//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! References of stored queries, named by OID: each relation of a range table, the columns expressions name, the types of constants and coercions, and bound routines.

use super::{DependencyBuilder, References};
use uqa_sql::plan::QueryPlan;
use uqa_sql::SQLError;

impl DependencyBuilder<'_> {
    /// What `query` references, as `recordDependencyOnExpr` finds it in a rule's action.
    pub(super) fn query_references(&self, query: &QueryPlan) -> Result<References, SQLError> {
        let found =
            super::super::view_definition::query_references(self.catalog, self.resolution, query)?;
        let mut references = References::default();
        let context = self.field_binding_context();
        for address in uqa_sql::binding::composite_dependencies::query_composite_dependencies(
            self.context.routines,
            query,
            &[],
            &context,
            None,
        )? {
            references.add(address);
        }
        for name in &found.relations {
            if let Some(oid) = self.objects.relation_oid_by_name(name) {
                references.add_relation(oid);
            }
        }
        for (relation, column) in &found.columns {
            let Some(oid) = self.objects.relation_oid_by_name(relation) else {
                continue;
            };
            if let Some(number) = self
                .objects
                .relation(oid)
                .and_then(|relation| relation.column_number(column))
            {
                references.add_column(oid, number);
            }
        }
        let expressions = self.expressions();
        for name in &found.types {
            if let Some(oid) = expressions.type_oid(name) {
                references.add_type(oid);
            }
        }
        for binding in &found.routines {
            if let Some(oid) = expressions.routine_oid(binding) {
                references.add_routine(oid);
            }
        }
        for (ty, oid) in &found.constants {
            super::expressions::add_constant_reference(ty, *oid, &mut references);
        }
        Ok(references)
    }

    pub(super) fn field_binding_context(&self) -> uqa_sql::binding::BindingContext<'_> {
        uqa_sql::binding::BindingContext {
            catalog: std::sync::Arc::new(self.catalog.clone()),
            resolution: self.resolution.clone(),
            ctes: std::collections::BTreeMap::new(),
            deferred_ctes: std::collections::BTreeMap::new(),
            non_returning_ctes: std::collections::BTreeSet::new(),
            scalar_subqueries: &[],
        }
    }
}
