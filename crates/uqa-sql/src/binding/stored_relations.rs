//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind durable statement relations while preserving namespace, catalog, and error ordering.

mod query;
pub use query::{
    bind_stored_query_relations, resolve_loaded_query_sequence, StoredQueryBindingContext,
    StoredQuerySequences,
};

use crate::{
    ast::{Expr, Statement},
    catalog::{
        events::RuleDependencies,
        resolution::{RelationLookupMode, RelationResolution},
        stored_ast::StoredAstVisitor,
    },
    SQLError,
};
use std::collections::BTreeSet;
use uqa_core::RelationIdentity;

pub trait StoredRelationCatalog {
    fn resolve_age_label_relation_name(&self, reference: &str) -> Result<Option<String>, SQLError>;
    fn resolve_visible_relation_kind(
        &self,
        reference: &str,
    ) -> Result<RelationResolution, SQLError>;
    fn resolve_loaded_visible_relation_kind(
        &self,
        reference: &str,
    ) -> Result<RelationResolution, SQLError>;
    fn resolve_bound_relation_kind(&self, reference: &str) -> Result<RelationResolution, SQLError>;
}

fn bind_catalog_relation_reference(
    catalog: &dyn StoredRelationCatalog,
    reference: &mut String,
    lookup_mode: RelationLookupMode,
    loaded_catalog: bool,
    context: &str,
    dependencies: &mut BTreeSet<RelationIdentity>,
) -> Result<(), SQLError> {
    if let Some(canonical) =
        crate::binding::view_dependencies::canonical_virtual_relation_reference(reference)
    {
        *reference = canonical;
        return Ok(());
    }
    if lookup_mode == RelationLookupMode::Dynamic {
        if let Some(canonical) = catalog.resolve_age_label_relation_name(reference)? {
            let relation = RelationIdentity::from_legacy_name(&canonical).map_err(|error| {
                SQLError::Internal(format!("decode bound rule source `{canonical}`: {error}"))
            })?;
            *reference = canonical;
            dependencies.insert(relation);
            return Ok(());
        }
    }
    let resolution = match (lookup_mode, loaded_catalog) {
        (RelationLookupMode::Dynamic, true) => {
            catalog.resolve_loaded_visible_relation_kind(reference)?
        }
        (RelationLookupMode::Dynamic, false) => catalog.resolve_visible_relation_kind(reference)?,
        (RelationLookupMode::Bound, _) => catalog.resolve_bound_relation_kind(reference)?,
    };
    let canonical = match resolution {
        RelationResolution::Found(
            canonical,
            "table" | "view" | "materialized view" | "foreign table" | "sequence",
        ) => canonical,
        RelationResolution::Found(canonical, kind) => {
            return Err(SQLError::Routine {
                sqlstate: "42809".into(),
                message: format!(
                    "{context} source \"{canonical}\" is a {kind}, not a row relation"
                ),
            });
        }
        RelationResolution::MissingSchema(schema) => {
            return Err(SQLError::Routine {
                sqlstate: "3F000".into(),
                message: format!("schema \"{schema}\" does not exist"),
            });
        }
        RelationResolution::MissingRelation => {
            return Err(SQLError::UnknownTable(reference.clone()));
        }
    };
    let relation = RelationIdentity::from_legacy_name(&canonical).map_err(|error| {
        SQLError::Internal(format!("decode bound rule source `{canonical}`: {error}"))
    })?;
    *reference = canonical;
    dependencies.insert(relation);
    Ok(())
}

pub fn bind_rule_action_relation_dependencies(
    catalog: &dyn StoredRelationCatalog,
    statement: &mut Statement,
    lookup_mode: RelationLookupMode,
) -> Result<RuleDependencies, SQLError> {
    let mut dependencies = BTreeSet::new();
    let mut bind = |reference: &mut String| {
        bind_catalog_relation_reference(
            catalog,
            reference,
            lookup_mode,
            false,
            "CREATE RULE",
            &mut dependencies,
        )
    };
    let mut ignore_routine = |_: &mut String,
                              _: Option<&mut Option<crate::ast::FunctionBinding>>|
     -> Result<(), SQLError> { Ok(()) };
    StoredAstVisitor {
        source: None,
        merge: None,
        expression: None,
        ty: None,
        relation: &mut bind,
        routine: &mut ignore_routine,
    }
    .bind_statement(statement)?;
    Ok(RuleDependencies {
        relations: dependencies,
        columns: BTreeSet::new(),
        routines: BTreeSet::new(),
    })
}

pub fn bind_rule_condition_relation_dependencies(
    catalog: &dyn StoredRelationCatalog,
    expression: &mut Expr,
    lookup_mode: RelationLookupMode,
) -> Result<RuleDependencies, SQLError> {
    let mut dependencies = BTreeSet::new();
    let mut bind = |reference: &mut String| {
        bind_catalog_relation_reference(
            catalog,
            reference,
            lookup_mode,
            false,
            "CREATE RULE",
            &mut dependencies,
        )
    };
    let mut ignore_routine = |_: &mut String,
                              _: Option<&mut Option<crate::ast::FunctionBinding>>|
     -> Result<(), SQLError> { Ok(()) };
    StoredAstVisitor {
        source: None,
        merge: None,
        expression: None,
        ty: None,
        relation: &mut bind,
        routine: &mut ignore_routine,
    }
    .bind_expr(expression, &BTreeSet::new())?;
    Ok(RuleDependencies {
        relations: dependencies,
        columns: BTreeSet::new(),
        routines: BTreeSet::new(),
    })
}

pub fn bind_stored_statement_relations(
    catalog: &dyn StoredRelationCatalog,
    statement: &mut Statement,
    lookup_mode: RelationLookupMode,
    loaded_catalog: bool,
    context: &str,
) -> Result<bool, SQLError> {
    let mut dependencies = BTreeSet::new();
    let mut changed = false;
    let mut bind = |reference: &mut String| {
        let previous = reference.clone();
        bind_catalog_relation_reference(
            catalog,
            reference,
            lookup_mode,
            loaded_catalog,
            context,
            &mut dependencies,
        )?;
        changed |= reference != &previous;
        Ok(())
    };
    let mut ignore_routine = |_: &mut String,
                              _: Option<&mut Option<crate::ast::FunctionBinding>>|
     -> Result<(), SQLError> { Ok(()) };
    StoredAstVisitor {
        source: None,
        merge: None,
        expression: None,
        ty: None,
        relation: &mut bind,
        routine: &mut ignore_routine,
    }
    .bind_statement(statement)?;
    match statement {
        Statement::Insert(insert) => {
            changed |= !insert.target_relation_bound;
            insert.target_relation_bound = true;
        }
        Statement::Update(update) => {
            changed |= !update.target_relation_bound;
            update.target_relation_bound = true;
        }
        Statement::Delete(delete) => {
            changed |= !delete.target_relation_bound;
            delete.target_relation_bound = true;
        }
        _ => {}
    }
    Ok(changed)
}
