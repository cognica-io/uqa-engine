//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind durable statement relations while preserving namespace, catalog, and error ordering.

mod production;
mod query;
pub(crate) use production::canonical_catalog_relation_with_control;
pub use query::{
    bind_stored_query_relations, resolve_loaded_query_sequence, StoredQueryBindingContext,
    StoredQueryNamespace, StoredQuerySequences,
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

    fn resolve_age_label_relation_name_with_control(
        &self,
        reference: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_core::memory::Produced<String>>, SQLError> {
        control.check()?;
        self.resolve_age_label_relation_name(reference)?
            .as_deref()
            .map(|name| control.copy_text(name).map_err(Into::into))
            .transpose()
    }
    fn resolve_visible_relation_kind_with_control(
        &self,
        reference: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_core::memory::Produced<RelationResolution>, SQLError> {
        control.check()?;
        copy_relation_resolution(&self.resolve_visible_relation_kind(reference)?, control)
    }
    fn resolve_loaded_visible_relation_kind_with_control(
        &self,
        reference: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_core::memory::Produced<RelationResolution>, SQLError> {
        control.check()?;
        copy_relation_resolution(
            &self.resolve_loaded_visible_relation_kind(reference)?,
            control,
        )
    }
    fn resolve_bound_relation_kind_with_control(
        &self,
        reference: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_core::memory::Produced<RelationResolution>, SQLError> {
        control.check()?;
        copy_relation_resolution(&self.resolve_bound_relation_kind(reference)?, control)
    }
}

fn copy_relation_resolution(
    resolution: &RelationResolution,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<uqa_core::memory::Produced<RelationResolution>, SQLError> {
    let (resolution, memory) = match resolution {
        RelationResolution::Found(name, kind) => {
            let (name, memory) = control.copy_text(name)?.into_parts();
            (RelationResolution::Found(name, kind), memory)
        }
        RelationResolution::MissingSchema(name) => {
            let (name, memory) = control.copy_text(name)?.into_parts();
            (RelationResolution::MissingSchema(name), memory)
        }
        RelationResolution::MissingRelation => (
            RelationResolution::MissingRelation,
            control.empty_reservation(),
        ),
    };
    Ok(control.finish(resolution, memory)?)
}

pub(crate) fn bind_catalog_relation_reference(
    catalog: &dyn StoredRelationCatalog,
    reference: &mut String,
    lookup_mode: RelationLookupMode,
    loaded_catalog: bool,
    context: &str,
    dependencies: &mut BTreeSet<RelationIdentity>,
) -> Result<(), SQLError> {
    let (canonical, age_label) = canonical_catalog_relation_with_control(
        catalog,
        reference,
        lookup_mode,
        loaded_catalog,
        context,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )?;
    let canonical = canonical
        .into_uncontrolled()
        .expect("ordinary relation binding");
    let relation = RelationIdentity::from_legacy_name(&canonical).map_err(|error| {
        SQLError::Internal(format!("decode bound rule source `{canonical}`: {error}"))
    })?;
    *reference = canonical;
    if age_label || crate::catalog::VirtualRelation::at(&relation.schema, &relation.name).is_none()
    {
        dependencies.insert(relation);
    }
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
        projection: None,
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
        projection: None,
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
        projection: None,
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
