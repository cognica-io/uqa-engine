//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Normalize durable constraint names and identities through a caller-owned identity allocator.
use std::collections::BTreeSet;
use uqa_core::RelationIdentity;
pub mod identity;

#[derive(Debug)]
pub enum ConstraintMetadataError {
    Invalid(String),
    Execution(Box<crate::SQLError>),
}

impl std::fmt::Display for ConstraintMetadataError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) => formatter.write_str(message),
            Self::Execution(error) => std::fmt::Display::fmt(error, formatter),
        }
    }
}

impl std::error::Error for ConstraintMetadataError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Invalid(_) => None,
            Self::Execution(error) => Some(error.as_ref()),
        }
    }
}
pub type ConstraintMetadataResult<T> = Result<T, ConstraintMetadataError>;
pub type CatalogIdentityAllocator<'a> = dyn CatalogObjectAllocator + 'a;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum CatalogOidClass {
    Constraint,
}

impl CatalogOidClass {
    pub const fn class_id(self) -> u32 {
        match self {
            Self::Constraint => 2606,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Constraint => "constraint",
        }
    }
}

/// Declaration normalization requests identities from its caller. Execution reserves public addresses; isolated declarations and initial migration can derive candidates before validating the complete catalog.
pub trait CatalogObjectAllocator {
    /// Validate the owning relation and reserve supplied addresses before allocating another row.
    fn include_catalog_identity(
        &mut self,
        _relation: &RelationIdentity,
        _class: CatalogOidClass,
        _identity: crate::ast::ConstraintCatalogIdentity,
    ) -> ConstraintMetadataResult<()> {
        Ok(())
    }

    fn allocate_object_id(&mut self, kind: &str) -> ConstraintMetadataResult<[u8; 16]>;

    fn allocate_catalog_oid(
        &mut self,
        class: CatalogOidClass,
        object_id: &[u8; 16],
    ) -> ConstraintMetadataResult<i64>;
}

impl<F> CatalogObjectAllocator for F
where
    F: FnMut(&str) -> ConstraintMetadataResult<[u8; 16]>,
{
    fn allocate_object_id(&mut self, kind: &str) -> ConstraintMetadataResult<[u8; 16]> {
        self(kind)
    }

    fn allocate_catalog_oid(
        &mut self,
        class: CatalogOidClass,
        object_id: &[u8; 16],
    ) -> ConstraintMetadataResult<i64> {
        Ok(crate::catalog::oids::stable_object_oid(
            class.label(),
            object_id,
        ))
    }
}

pub fn materialize_constraint_metadata(
    relation: &RelationIdentity,
    columns: &mut [crate::ast::ColumnDef],
    constraints: &mut crate::ast::TableConstraintSet,
    allocate: &mut CatalogIdentityAllocator<'_>,
) -> ConstraintMetadataResult<bool> {
    identity::claims::validate_present_identities(columns, constraints)?;
    for identity in identity::claims::identities(columns, constraints) {
        allocate.include_catalog_identity(relation, CatalogOidClass::Constraint, identity)?;
    }
    // Releases predating typed table-key persistence stored column-level PRIMARY KEY and UNIQUE declarations only as ColumnDef flags. Promote those legacy flags before assigning names so catalog publication always sees named constraints.
    let mut changed = promote_legacy_column_key_constraints(columns, constraints);
    let mut used = existing_constraint_names(columns, constraints)?;

    let mut column_object_ids = BTreeSet::new();
    for column in columns.iter_mut() {
        if column
            .object_id
            .is_some_and(|object_id| !column_object_ids.insert(object_id))
        {
            column.object_id = None;
        }
        changed |= assign_catalog_object_id(&mut column.object_id, "column", allocate)?;
        if let Some(object_id) = column.object_id {
            column_object_ids.insert(object_id);
        }
        if column.not_null {
            changed |= assign_constraint_name(
                &mut column.not_null_name,
                format!("{}_{}_not_null", relation.name, column.name),
                &mut used,
            )?;
            changed |= identity::materialize_not_null_identity(column, allocate)?;
        }
        if column.check.is_some() {
            changed |= assign_constraint_name(
                &mut column.check_name,
                format!("{}_{}_check", relation.name, column.name),
                &mut used,
            )?;
            changed |= assign_catalog_object_id(
                &mut column.check_object_id,
                "CHECK constraint",
                allocate,
            )?;
            changed |= identity::materialize_check_oid(
                column.check_object_id,
                &mut column.check_catalog_oid,
                allocate,
            )?;
        }
        if let Some(reference) = &mut column.references {
            changed |= assign_constraint_name(
                &mut reference.name,
                format!("{}_{}_fkey", relation.name, column.name),
                &mut used,
            )?;
            changed |= assign_constraint_object_id(&mut reference.object_id, allocate)?;
            changed |=
                identity::foreign_keys::materialize(&mut reference.catalog_identity, allocate)?;
        }
    }
    for constraint in &mut constraints.key_constraints {
        let base = match constraint.kind {
            crate::ast::TableKeyConstraintKind::PrimaryKey => {
                format!("{}_pkey", relation.name)
            }
            crate::ast::TableKeyConstraintKind::Unique => format!(
                "{}_{}_key",
                relation.name,
                constraint_column_component(&constraint.columns, relation)?
            ),
        };
        changed |= assign_constraint_name(&mut constraint.name, base, &mut used)?;
        changed |= identity::materialize_key_identity(constraint, allocate)?;
    }
    for constraint in &mut constraints.checks {
        let mut referenced_columns = Vec::new();
        collect_constraint_columns(&constraint.expr, &mut referenced_columns);
        let base = if referenced_columns.len() == 1 {
            format!("{}_{}_check", relation.name, referenced_columns[0])
        } else {
            format!("{}_check", relation.name)
        };
        changed |= assign_constraint_name(&mut constraint.name, base, &mut used)?;
        changed |=
            assign_catalog_object_id(&mut constraint.object_id, "CHECK constraint", allocate)?;
        changed |= identity::materialize_check_oid(
            constraint.object_id,
            &mut constraint.catalog_oid,
            allocate,
        )?;
    }
    changed |= synchronize_partition_inherited_foreign_key_ids(constraints);
    for constraint in &mut constraints.foreign_keys {
        let component = constraint_column_component(&constraint.local_columns, relation)?;
        changed |= assign_constraint_name(
            &mut constraint.name,
            format!("{}_{}_fkey", relation.name, component),
            &mut used,
        )?;
        changed |= assign_constraint_object_id(&mut constraint.object_id, allocate)?;
        changed |= identity::foreign_keys::materialize(&mut constraint.catalog_identity, allocate)?;
    }
    changed |= synchronize_partition_inherited_foreign_key_ids(constraints);
    changed |= identity::keys::synchronize_provenance(constraints);
    identity::claims::validate_constraint_identities(columns, constraints)?;
    Ok(changed)
}

fn existing_constraint_names(
    columns: &[crate::ast::ColumnDef],
    constraints: &crate::ast::TableConstraintSet,
) -> ConstraintMetadataResult<BTreeSet<String>> {
    let mut used = BTreeSet::new();
    for column in columns {
        record_constraint_name(&mut used, column.not_null_name.as_deref())?;
        record_constraint_name(&mut used, column.check_name.as_deref())?;
        record_constraint_name(
            &mut used,
            column
                .references
                .as_ref()
                .and_then(|reference| reference.name.as_deref()),
        )?;
    }
    for constraint in &constraints.key_constraints {
        record_constraint_name(&mut used, constraint.name.as_deref())?;
    }
    for constraint in &constraints.checks {
        record_constraint_name(&mut used, constraint.name.as_deref())?;
    }
    for constraint in &constraints.foreign_keys {
        record_constraint_name(&mut used, constraint.name.as_deref())?;
    }
    Ok(used)
}

fn promote_legacy_column_key_constraints(
    columns: &[crate::ast::ColumnDef],
    constraints: &mut crate::ast::TableConstraintSet,
) -> bool {
    let mut changed = false;
    for column in columns {
        for (present, kind) in [
            (
                column.primary_key,
                crate::ast::TableKeyConstraintKind::PrimaryKey,
            ),
            (column.unique, crate::ast::TableKeyConstraintKind::Unique),
        ] {
            if !present
                || constraints.key_constraints.iter().any(|constraint| {
                    constraint.kind == kind
                        && constraint.columns.as_slice() == [column.name.as_str()]
                })
            {
                continue;
            }
            constraints
                .key_constraints
                .push(crate::ast::TableKeyConstraint {
                    catalog_identity: None,
                    name: None,
                    kind,
                    columns: vec![column.name.clone()],
                    nulls_not_distinct: false,
                    without_overlaps: false,
                });
            changed = true;
        }
    }
    changed
}

pub fn foreign_keys_match_without_object_id(
    left: &crate::ast::ForeignKey,
    right: &crate::ast::ForeignKey,
) -> bool {
    let mut left = left.clone();
    let mut right = right.clone();
    left.object_id = None;
    right.object_id = None;
    left.catalog_identity = None;
    right.catalog_identity = None;
    left == right
}

/// Attachment provenance tracks one local row even after its name or enforcement flags change. Legacy entries may still lack the independent catalog identity.
pub fn foreign_key_provenance_matches(
    left: &crate::ast::ForeignKey,
    right: &crate::ast::ForeignKey,
) -> bool {
    match (left.catalog_identity, right.catalog_identity) {
        (Some(left), Some(right)) => left == right,
        _ => {
            (left.object_id.is_some() && left.object_id == right.object_id)
                || foreign_keys_match_without_object_id(left, right)
        }
    }
}

pub fn synchronize_partition_inherited_foreign_key_ids(
    constraints: &mut crate::ast::TableConstraintSet,
) -> bool {
    let mut changed = false;
    for inherited_index in 0..constraints.hierarchy.partition_inherited_foreign_keys.len() {
        let inherited = &constraints.hierarchy.partition_inherited_foreign_keys[inherited_index];
        let Some(foreign_key_index) = constraints
            .foreign_keys
            .iter()
            .position(|foreign_key| foreign_key_provenance_matches(foreign_key, inherited))
        else {
            continue;
        };
        let object_id = constraints.foreign_keys[foreign_key_index]
            .object_id
            .or(inherited.object_id);
        if constraints.foreign_keys[foreign_key_index].object_id != object_id {
            constraints.foreign_keys[foreign_key_index].object_id = object_id;
            changed = true;
        }
        if constraints.hierarchy.partition_inherited_foreign_keys[inherited_index].object_id
            != object_id
        {
            constraints.hierarchy.partition_inherited_foreign_keys[inherited_index].object_id =
                object_id;
            changed = true;
        }
        let catalog_identity = constraints.foreign_keys[foreign_key_index].catalog_identity;
        if constraints.hierarchy.partition_inherited_foreign_keys[inherited_index].catalog_identity
            != catalog_identity
        {
            constraints.hierarchy.partition_inherited_foreign_keys[inherited_index]
                .catalog_identity = catalog_identity;
            changed = true;
        }
    }
    changed
}

fn assign_constraint_object_id(
    target: &mut Option<[u8; 16]>,
    allocate: &mut CatalogIdentityAllocator<'_>,
) -> ConstraintMetadataResult<bool> {
    assign_catalog_object_id(target, "foreign-key constraint", allocate)
}

fn assign_catalog_object_id(
    target: &mut Option<[u8; 16]>,
    object_kind: &str,
    allocate: &mut CatalogIdentityAllocator<'_>,
) -> ConstraintMetadataResult<bool> {
    if target.is_some() {
        return Ok(false);
    }
    *target = Some(allocate.allocate_object_id(object_kind)?);
    Ok(true)
}

fn record_constraint_name(
    used: &mut BTreeSet<String>,
    name: Option<&str>,
) -> ConstraintMetadataResult<()> {
    let Some(name) = name else {
        return Ok(());
    };
    if name.is_empty() {
        return Err(ConstraintMetadataError::Invalid(
            "constraint name must not be empty".into(),
        ));
    }
    if !used.insert(name.to_string()) {
        return Err(ConstraintMetadataError::Invalid(format!(
            "constraint `{name}` is declared more than once"
        )));
    }
    Ok(())
}

fn assign_constraint_name(
    target: &mut Option<String>,
    base: String,
    used: &mut BTreeSet<String>,
) -> ConstraintMetadataResult<bool> {
    if target.is_some() {
        return Ok(false);
    }
    if used.insert(base.clone()) {
        *target = Some(base);
        return Ok(true);
    }
    for suffix in 1_u64.. {
        let candidate = format!("{base}{suffix}");
        if used.insert(candidate.clone()) {
            *target = Some(candidate);
            return Ok(true);
        }
    }
    Err(ConstraintMetadataError::Invalid(format!(
        "constraint name suffix space exhausted for `{base}`"
    )))
}

fn constraint_column_component(
    columns: &[String],
    relation: &RelationIdentity,
) -> ConstraintMetadataResult<String> {
    if columns.is_empty() {
        return Err(ConstraintMetadataError::Invalid(format!(
            "constraint on table `{}` has no columns",
            relation.qualified_name()
        )));
    }
    Ok(columns.join("_"))
}

fn collect_constraint_columns(expression: &crate::ast::Expr, output: &mut Vec<String>) {
    use crate::ast::{Expr, FrameBound};
    match expression {
        Expr::Column(name) | Expr::QualifiedColumn { column: name, .. } => {
            if !output.contains(name) {
                output.push(name.clone());
            }
        }
        Expr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            for argument in args {
                collect_constraint_columns(argument, output);
            }
            for order in order_by {
                collect_constraint_columns(&order.expr, output);
            }
            if let Some(filter) = filter {
                collect_constraint_columns(filter, output);
            }
        }
        Expr::Array(items) | Expr::Row(items) | Expr::And(items) | Expr::Or(items) => {
            for item in items {
                collect_constraint_columns(item, output);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            collect_constraint_columns(lhs, output);
            collect_constraint_columns(rhs, output);
        }
        Expr::Not(inner)
        | Expr::UnaryMinus(inner)
        | Expr::IsNull { expr: inner, .. }
        | Expr::Cast { expr: inner, .. } => {
            collect_constraint_columns(inner, output);
        }
        Expr::Between { expr, low, high } => {
            collect_constraint_columns(expr, output);
            collect_constraint_columns(low, output);
            collect_constraint_columns(high, output);
        }
        Expr::InList { expr, list, .. } => {
            collect_constraint_columns(expr, output);
            for item in list {
                collect_constraint_columns(item, output);
            }
        }
        Expr::WindowCall { args, spec, .. } => {
            for argument in args {
                collect_constraint_columns(argument, output);
            }
            for expression in &spec.partition_by {
                collect_constraint_columns(expression, output);
            }
            for order in &spec.order_by {
                collect_constraint_columns(&order.expr, output);
            }
            if let Some(frame) = &spec.frame {
                for bound in [&frame.start, &frame.end] {
                    if let FrameBound::Preceding(expression) | FrameBound::Following(expression) =
                        bound
                    {
                        collect_constraint_columns(expression, output);
                    }
                }
            }
        }
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(base) = base {
                collect_constraint_columns(base, output);
            }
            for (condition, result) in when {
                collect_constraint_columns(condition, output);
                collect_constraint_columns(result, output);
            }
            if let Some(else_branch) = else_branch {
                collect_constraint_columns(else_branch, output);
            }
        }
        Expr::InSubquery { expr, .. } => collect_constraint_columns(expr, output),
        Expr::Default
        | Expr::Star
        | Expr::QualifiedStar(_)
        | Expr::InternalColumn(_)
        | Expr::Literal(_)
        | Expr::TypedLiteral { .. }
        | Expr::Param(_)
        | Expr::ScalarSubquery(_)
        | Expr::Exists { .. } => {}
    }
}
