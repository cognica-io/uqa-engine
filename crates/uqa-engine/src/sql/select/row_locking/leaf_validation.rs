//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    bind_source_plan_schema, virtual_row_lockable, CatalogReadView, ComputePlan, CteScope, Engine,
    LockStrength, PhysicalRow, QueryBlockPlan, QueryPlan, RelationNameResolution, RelationalPlan,
    RowProjectionValue, RowSchema, SQLError, SQLParam, SourcePlan,
};

pub(super) struct LockLeaf {
    pub(super) names: Vec<String>,
    pub(super) qualifier: String,
    pub(super) storage_name: String,
    pub(super) display_name: String,
    pub(super) kind: LockLeafKind,
    pub(super) nullable: bool,
}

pub(super) enum LockLeafKind {
    Base,
    View(Box<QueryPlan>),
    Subquery(Box<QueryPlan>),
    Cte,
    Values,
    Function,
    Foreign,
    Virtual { lockable: bool },
}

impl LockLeafKind {
    pub(super) fn implicitly_lockable(&self) -> bool {
        matches!(
            self,
            Self::Base | Self::View(_) | Self::Subquery(_) | Self::Foreign | Self::Virtual { .. }
        )
    }

    pub(super) fn carries_row_identity(&self) -> bool {
        matches!(self, Self::Base | Self::View(_) | Self::Subquery(_))
    }

    pub(super) fn is_identity_source(&self) -> bool {
        matches!(self, Self::View(_) | Self::Subquery(_))
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves lock target and recheck order"
)]
pub(super) fn collect_source_leaves(
    source: &SourcePlan,
    nullable: bool,
    ctes: &CteScope,
) -> Result<Vec<LockLeaf>, SQLError> {
    match source {
        SourcePlan::Table {
            name,
            qualifier,
            alias,
            include_descendants,
        } => {
            let visible = alias.as_deref().unwrap_or(qualifier);
            let mut names = vec![visible.to_string()];
            if alias.is_none() {
                push_unique(&mut names, name);
                if let Some((_, local)) = name.rsplit_once('.') {
                    push_unique(&mut names, local);
                }
            }
            let catalog = ctes.catalog_read_view()?;
            let resolution = ctes.relation_name_resolution()?;
            let kind = classify_table_leaf(&catalog, &resolution, name, ctes)?;
            if matches!(kind, LockLeafKind::Base) {
                return Ok(catalog
                    .hierarchy_scan_tables(&resolution, name, *include_descendants)?
                    .into_iter()
                    .map(|storage_name| LockLeaf {
                        names: names.clone(),
                        qualifier: visible.to_string(),
                        storage_name,
                        display_name: visible.to_string(),
                        kind: LockLeafKind::Base,
                        nullable,
                    })
                    .collect());
            }
            Ok(vec![LockLeaf {
                names,
                qualifier: visible.to_string(),
                storage_name: name.clone(),
                display_name: visible.to_string(),
                kind,
                nullable,
            }])
        }
        SourcePlan::Join {
            left, right, kind, ..
        } => {
            let (left_nullable, right_nullable) = match kind {
                uqa_sql::ast::JoinKind::Left => (nullable, true),
                uqa_sql::ast::JoinKind::Right => (true, nullable),
                uqa_sql::ast::JoinKind::Full => (true, true),
                uqa_sql::ast::JoinKind::Inner | uqa_sql::ast::JoinKind::Cross => {
                    (nullable, nullable)
                }
            };
            let mut leaves = collect_source_leaves(left, left_nullable, ctes)?;
            leaves.extend(collect_source_leaves(right, right_nullable, ctes)?);
            Ok(leaves)
        }
        SourcePlan::Values { alias, .. } => Ok(vec![LockLeaf {
            names: alias.iter().cloned().collect(),
            qualifier: alias.clone().unwrap_or_default(),
            storage_name: String::new(),
            display_name: alias.clone().unwrap_or_else(|| "values".into()),
            kind: LockLeafKind::Values,
            nullable,
        }]),
        SourcePlan::Function {
            name,
            output_name,
            alias,
            ..
        } => {
            let visible = alias.as_deref().unwrap_or(output_name);
            Ok(vec![LockLeaf {
                names: vec![visible.to_string(), output_name.clone(), name.clone()],
                qualifier: visible.to_string(),
                storage_name: String::new(),
                display_name: visible.to_string(),
                kind: LockLeafKind::Function,
                nullable,
            }])
        }
        SourcePlan::FunctionGroup {
            functions, alias, ..
        } => {
            let first = functions
                .first()
                .ok_or_else(|| SQLError::Internal("ROWS FROM group has no functions".into()))?;
            let visible = alias.as_deref().unwrap_or(&first.output_name);
            Ok(vec![LockLeaf {
                names: vec![visible.to_string()],
                qualifier: visible.to_string(),
                storage_name: String::new(),
                display_name: visible.to_string(),
                kind: LockLeafKind::Function,
                nullable,
            }])
        }
        SourcePlan::Subquery { body, alias, .. } => {
            let visible = alias.clone().unwrap_or_default();
            Ok(vec![LockLeaf {
                names: if visible.is_empty() {
                    Vec::new()
                } else {
                    vec![visible.clone()]
                },
                qualifier: visible.clone(),
                storage_name: String::new(),
                display_name: if visible.is_empty() {
                    "subquery".into()
                } else {
                    visible
                },
                kind: LockLeafKind::Subquery(body.clone()),
                nullable,
            }])
        }
    }
}

pub(super) fn collect_source_leaf_plans<'a>(
    source: &'a SourcePlan,
    path: &mut Vec<u8>,
    leaves: &mut Vec<(Vec<u8>, &'a SourcePlan)>,
) {
    match source {
        SourcePlan::Join { left, right, .. } => {
            path.push(0);
            collect_source_leaf_plans(left, path, leaves);
            path.pop();
            path.push(1);
            collect_source_leaf_plans(right, path, leaves);
            path.pop();
        }
        SourcePlan::Table { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. }
        | SourcePlan::Subquery { .. } => leaves.push((path.clone(), source)),
    }
}

#[cold]
#[inline(never)]
pub(super) fn copy_recheck_source_row(
    engine: &Engine,
    source: &SourcePlan,
    qualifier: &str,
    candidate_schema: &RowSchema,
    candidate: &PhysicalRow,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<(RowSchema, PhysicalRow), SQLError> {
    let (schema, slots) = if qualifier.is_empty() {
        let source_schema = bind_source_plan_schema(engine, source, params, ctes, None)?;
        let slots = source_schema
            .identities()
            .iter()
            .map(|identity| {
                candidate_schema
                    .physical_slot_for_identity(identity)
                    .ok_or_else(|| {
                        SQLError::Internal(format!(
                            "row-lock recheck cannot identify unqualified copy-row column `{}`",
                            identity.column()
                        ))
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        (
            RowSchema::with_identities(
                source_schema.columns().to_vec(),
                source_schema.identities().to_vec(),
                source_schema.column_types().to_vec(),
            ),
            slots,
        )
    } else {
        let layout = candidate_schema.qualified_star_layout(qualifier);
        let columns = layout
            .iter()
            .map(|(column, _, _)| column.clone())
            .collect::<Vec<_>>();
        let types = layout
            .iter()
            .map(|(_, _, column_type)| column_type.clone())
            .collect::<Vec<_>>();
        let slots = layout
            .into_iter()
            .map(|(_, slot, _)| slot)
            .collect::<Vec<_>>();
        (
            RowSchema::with_qualified_types(qualifier, columns, types),
            slots,
        )
    };
    let row = candidate
        .project_with_values(slots.into_iter().map(RowProjectionValue::InputSlot))
        .without_lock_origins();
    Ok((schema, row))
}

pub(super) fn classify_table_leaf(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    name: &str,
    ctes: &CteScope,
) -> Result<LockLeafKind, SQLError> {
    if ctes.is_visible_cte(name) {
        return Ok(LockLeafKind::Cte);
    }
    if let Some(view) = catalog.view_resolved(resolution, name)? {
        return Ok(LockLeafKind::View(Box::new(view.query.clone())));
    }
    if catalog.foreign_table_resolved(resolution, name)?.is_some() {
        return Ok(LockLeafKind::Foreign);
    }
    if catalog.table(resolution, name)?.is_some() {
        return Ok(LockLeafKind::Base);
    }
    let lockable = virtual_row_lockable(resolution, name).unwrap_or(false);
    Ok(LockLeafKind::Virtual { lockable })
}

pub(super) fn reject_unusable_lock_leaf(
    engine: &Engine,
    source: &LockLeaf,
    strength: LockStrength,
) -> Result<(), SQLError> {
    match &source.kind {
        LockLeafKind::Values => Ok(()),
        LockLeafKind::Function => Err(SQLError::Unsupported(
            "FOR UPDATE/SHARE cannot be applied to a function".into(),
        )),
        LockLeafKind::Cte => Err(SQLError::Unsupported(format!(
            "{} cannot be applied to a WITH query",
            strength.sql_name()
        ))),
        LockLeafKind::Foreign => Err(SQLError::Unsupported(format!(
            "{} cannot be applied to foreign table \"{}\"",
            strength.sql_name(),
            source.display_name
        ))),
        LockLeafKind::Virtual { lockable: true } => {
            reject_nullable_lock_source(source.nullable, strength)
        }
        LockLeafKind::Virtual { lockable: false } => Err(SQLError::Unsupported(format!(
            "{} cannot be applied to relation \"{}\"",
            strength.sql_name(),
            source.display_name
        ))),
        LockLeafKind::Base => reject_nullable_lock_source(source.nullable, strength),
        LockLeafKind::View(ref plan) => {
            validate_view_locking(engine, plan, strength, source.nullable)
        }
        LockLeafKind::Subquery(plan) => {
            validate_propagated_query(engine, plan, strength, source.nullable, false)
        }
    }
}

pub(super) fn reject_nullable_lock_source(
    nullable: bool,
    strength: LockStrength,
) -> Result<(), SQLError> {
    if nullable {
        return Err(SQLError::Unsupported(format!(
            "{} cannot be applied to the nullable side of an outer join",
            strength.sql_name()
        )));
    }
    Ok(())
}

pub(super) fn validate_view_locking(
    engine: &Engine,
    plan: &QueryPlan,
    strength: LockStrength,
    nullable: bool,
) -> Result<(), SQLError> {
    validate_propagated_query(engine, plan, strength, nullable, true)
}

pub(super) fn validate_propagated_query(
    engine: &Engine,
    plan: &QueryPlan,
    strength: LockStrength,
    nullable: bool,
    allow_set_operation_root: bool,
) -> Result<(), SQLError> {
    match &plan.root {
        RelationalPlan::SetOp { .. } if allow_set_operation_root => Ok(()),
        RelationalPlan::SetOp { .. } => Err(SQLError::Unsupported(format!(
            "{} is not allowed with UNION/INTERSECT/EXCEPT",
            strength.sql_name()
        ))),
        RelationalPlan::Values { .. } => Ok(()),
        RelationalPlan::QueryBlock(block) => {
            validate_locking_block_shape(block, strength)?;
            let Some(source) = block.from.as_ref() else {
                return Ok(());
            };
            validate_propagated_source(engine, source, strength, nullable, plan)
        }
    }
}

pub(super) fn validate_locking_block_shape(
    block: &QueryBlockPlan,
    strength: LockStrength,
) -> Result<(), SQLError> {
    let label = strength.sql_name();
    if block.distinct || !block.distinct_on.is_empty() {
        return Err(SQLError::Unsupported(format!(
            "{label} is not allowed with DISTINCT clause"
        )));
    }
    if !block.group_by.is_empty() || !block.grouping_sets.is_empty() {
        return Err(SQLError::Unsupported(format!(
            "{label} is not allowed with GROUP BY clause"
        )));
    }
    if block.having.is_some() {
        return Err(SQLError::Unsupported(format!(
            "{label} is not allowed with HAVING clause"
        )));
    }
    if matches!(block.compute, ComputePlan::Window)
        || block
            .order_by
            .iter()
            .any(|ordering| ordering.expr.contains_window())
    {
        return Err(SQLError::Unsupported(format!(
            "{label} is not allowed with window functions"
        )));
    }
    if matches!(block.compute, ComputePlan::Aggregate) {
        return Err(SQLError::Unsupported(format!(
            "{label} is not allowed with aggregate functions"
        )));
    }
    Ok(())
}

pub(super) fn validate_propagated_source(
    engine: &Engine,
    source: &SourcePlan,
    strength: LockStrength,
    nullable: bool,
    owner: &QueryPlan,
) -> Result<(), SQLError> {
    match source {
        SourcePlan::Table { name, .. } => {
            if owner.ctes.iter().any(|cte| cte.name == *name) || engine.view_plan(name)?.is_some() {
                return Ok(());
            }
            reject_nullable_lock_source(nullable, strength)
        }
        SourcePlan::Join {
            left, right, kind, ..
        } => {
            let (left_nullable, right_nullable) = match kind {
                uqa_sql::ast::JoinKind::Left => (nullable, true),
                uqa_sql::ast::JoinKind::Right => (true, nullable),
                uqa_sql::ast::JoinKind::Full => (true, true),
                uqa_sql::ast::JoinKind::Inner | uqa_sql::ast::JoinKind::Cross => {
                    (nullable, nullable)
                }
            };
            validate_propagated_source(engine, left, strength, left_nullable, owner)?;
            validate_propagated_source(engine, right, strength, right_nullable, owner)
        }
        SourcePlan::Subquery { body, .. } => {
            validate_propagated_query(engine, body, strength, nullable, false)
        }
        SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. } => Ok(()),
    }
}

pub(super) fn push_unique(names: &mut Vec<String>, name: &str) {
    if !name.is_empty() && !names.iter().any(|existing| existing == name) {
        names.push(name.to_string());
    }
}
