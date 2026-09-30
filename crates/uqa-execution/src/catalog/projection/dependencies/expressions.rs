//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! References of stored expression syntax as `find_expr_references_walker` finds them in the analyzed expression. Stored syntax keeps what analysis decided: user-defined types by identity in casts and constants, routines by identity in calls, and `reg*` constants by OID.

use super::{CatalogObjects, References, RelationObject};
use crate::catalog::context::CatalogContext;
use std::collections::BTreeSet;
use uqa_core::Value;
use uqa_sql::ast::{ColumnDef, ColumnType, Expr, FunctionBinding};
use uqa_sql::catalog::dependencies::{ObjectAddress, NAMESPACE_CLASS};
use uqa_sql::catalog::stored_ast::StoredAstVisitor;
use uqa_sql::SQLError;

/// The columns an expression may name.
#[derive(Clone, Copy)]
pub(super) enum ColumnScope<'a> {
    /// No columns: domain constraints and defaults, and routine parameter defaults.
    None,
    /// The columns of one relation, as constraints, defaults, generation expressions, index keys and predicates, and partition keys name them.
    Relation(u32, &'a RelationObject),
    /// The columns of a trigger's relation, which its `WHEN` condition names through `OLD` and `NEW`.
    Trigger(u32, &'a RelationObject),
}

/// Expression syntax, walked for what it references in one catalog snapshot.
pub(super) struct ExpressionReferences<'a> {
    pub context: &'a CatalogContext<'a>,
    pub objects: &'a CatalogObjects,
}

#[derive(Default)]
struct Found {
    columns: Vec<(Option<String>, String)>,
    types: Vec<String>,
    routines: Vec<FunctionBinding>,
    constants: Vec<(String, i64)>,
}

impl ExpressionReferences<'_> {
    /// Add what `expression` references to `references`.
    pub(super) fn collect(
        &self,
        expression: &Expr,
        scope: ColumnScope<'_>,
        references: &mut References,
    ) -> Result<(), SQLError> {
        let mut found = Found::default();
        let mut expression_nodes = |node: &mut Expr| -> Result<(), SQLError> {
            match node {
                Expr::Column(name) => found.columns.push((None, name.clone())),
                Expr::QualifiedColumn { qualifier, column } => found
                    .columns
                    .push((Some(qualifier.clone()), column.clone())),
                Expr::TypedLiteral {
                    value: Value::Int(oid),
                    ty,
                } => found.constants.push((ty.clone(), *oid)),
                _ => {}
            }
            Ok(())
        };
        let mut types = Vec::new();
        let mut type_names = |name: &mut String| types.push(name.clone());
        let mut relation = |_: &mut String| -> Result<(), SQLError> { Ok(()) };
        let mut routines = Vec::new();
        let mut routine = |_: &mut String,
                           binding: Option<&mut Option<FunctionBinding>>|
         -> Result<(), SQLError> {
            if let Some(Some(binding)) = binding {
                routines.push(binding.clone());
            }
            Ok(())
        };
        StoredAstVisitor {
            source: None,
            merge: None,
            expression: Some(&mut expression_nodes),
            ty: Some(&mut type_names),
            relation: &mut relation,
            routine: &mut routine,
        }
        .bind_expr(&mut expression.clone(), &BTreeSet::new())?;
        found.types = types;
        found.routines = routines;
        self.add_found(found, scope, references);
        self.collect_sequence_arguments(expression, references)?;
        Ok(())
    }

    /// The sequence a sequence function names by a constant, which analysis turns into a `regclass` constant.
    fn collect_sequence_arguments(
        &self,
        expression: &Expr,
        references: &mut References,
    ) -> Result<(), SQLError> {
        let mut names = Vec::new();
        uqa_sql::schema::dependencies::rewrites::rewrite_sequence_function_references(
            &mut expression.clone(),
            &mut |name| {
                names.push(name.clone());
                Ok(())
            },
        )
        .map_err(SQLError::Internal)?;
        for name in names {
            if let Some(oid) = self.objects.relation_oid_by_name(&name) {
                references.add_relation(oid);
            }
        }
        Ok(())
    }

    /// Add what `expression` references once it is coerced to `target`, as `cookDefault` and `coerce_to_specific_type` coerce defaults and generation expressions: a value of another type, or a literal of unknown type, is converted by a node whose result has the target type.
    pub(super) fn collect_assigned(
        &self,
        expression: &Expr,
        target: &ColumnType,
        (scope, columns): (ColumnScope<'_>, &[ColumnDef]),
        references: &mut References,
    ) -> Result<(), SQLError> {
        self.collect(expression, scope, references)?;
        let plan = uqa_sql::plan::ExpressionPlan::lower(expression.clone());
        let schema = uqa_sql::schema::ColumnTypeSchema::new(columns);
        let found = uqa_sql::type_resolution::scalar_type_with_resolver(
            &plan.scalar,
            &schema,
            &[],
            self.context.routines,
        )?;
        let target_oid = uqa_sql::catalog::type_metadata::pg_type_oid(target);
        if found
            .as_ref()
            .is_none_or(|found| uqa_sql::catalog::type_metadata::pg_type_oid(found) != target_oid)
        {
            if let Ok(oid) = u32::try_from(target_oid) {
                references.add_type(oid);
            }
        }
        Ok(())
    }

    fn add_found(&self, found: Found, scope: ColumnScope<'_>, references: &mut References) {
        for (qualifier, name) in found.columns {
            match scope {
                ColumnScope::Relation(oid, relation) => {
                    if let Some(column) = relation.column_number(&name) {
                        references.add_column(oid, column);
                    }
                }
                ColumnScope::Trigger(oid, relation) => {
                    let transition = qualifier.as_deref().is_some_and(|qualifier| {
                        qualifier.eq_ignore_ascii_case("new")
                            || qualifier.eq_ignore_ascii_case("old")
                    });
                    if let (true, Some(column)) = (transition, relation.column_number(&name)) {
                        references.add_column(oid, column);
                    }
                }
                ColumnScope::None => {}
            }
        }
        for name in found.types {
            if let Some(oid) = self.type_oid(&name) {
                references.add_type(oid);
            }
        }
        for binding in &found.routines {
            if let Some(oid) = self.routine_oid(binding) {
                references.add_routine(oid);
            }
        }
        for (ty, oid) in found.constants {
            add_constant_reference(&ty, oid, references);
        }
    }

    /// The `pg_type` OID of a type named in stored syntax.
    pub(super) fn type_oid(&self, name: &str) -> Option<u32> {
        let ty = crate::catalog::projection::resolve_catalog_column_type(self.context, name)?;
        u32::try_from(uqa_sql::catalog::type_metadata::pg_type_oid(&ty)).ok()
    }

    /// The `pg_proc` OID of a user routine a call is bound to; built-in routines are pinned.
    pub(super) fn routine_oid(&self, binding: &FunctionBinding) -> Option<u32> {
        if binding.builtin {
            return None;
        }
        self.objects.routine_oid(binding.object_id.as_ref()?)
    }
}

/// A `reg*` constant references the object it names, as a `Const` of an OID alias type does.
pub(super) fn add_constant_reference(ty: &str, oid: i64, references: &mut References) {
    let Ok(oid) = u32::try_from(oid) else {
        return;
    };
    let name = ty.strip_prefix("pg_catalog.").unwrap_or(ty);
    match name.to_ascii_lowercase().as_str() {
        "regclass" => references.add_relation(oid),
        "regtype" => references.add_type(oid),
        "regproc" | "regprocedure" => references.add_routine(oid),
        "regnamespace" => references.add(ObjectAddress::whole(NAMESPACE_CLASS, oid)),
        _ => {}
    }
}
