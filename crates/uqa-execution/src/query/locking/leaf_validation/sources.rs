//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrow lock-target declarations and admit only selected storage names and leaf slots.

use super::{QueryPlan, SQLError, SourcePlan};
use crate::catalog::{CatalogReadView, RelationNameResolution};
use crate::query::CteScope;
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

pub(in crate::query::locking) struct LockLeaf<'a> {
    pub(in crate::query::locking) names: [Option<&'a str>; 3],
    pub(in crate::query::locking) qualifier: &'a str,
    pub(in crate::query::locking) storage_name: Produced<String>,
    pub(in crate::query::locking) display_name: &'a str,
    pub(in crate::query::locking) kind: LockLeafKind<'a>,
    pub(in crate::query::locking) nullable: bool,
}

pub(in crate::query::locking) enum LockLeafKind<'a> {
    Base,
    View(&'a QueryPlan),
    Subquery(&'a QueryPlan),
    Cte,
    Values,
    Function,
    Foreign,
    Virtual { lockable: bool },
}

impl LockLeafKind<'_> {
    pub(in crate::query::locking) fn implicitly_lockable(&self) -> bool {
        matches!(
            self,
            Self::Base | Self::View(_) | Self::Subquery(_) | Self::Foreign | Self::Virtual { .. }
        )
    }
    pub(in crate::query::locking) fn carries_row_identity(&self) -> bool {
        matches!(self, Self::Base | Self::View(_) | Self::Subquery(_))
    }
    pub(in crate::query::locking) fn is_identity_source(&self) -> bool {
        matches!(self, Self::View(_) | Self::Subquery(_))
    }
}

struct LeafCollector<'a, 'c, S: Clone> {
    catalog: &'a CatalogReadView,
    resolution: &'c RelationNameResolution,
    ctes: &'c CteScope<S>,
    output: ProductionVec<'c, LockLeaf<'a>>,
    control: ProductionControl<'c>,
}

pub(in crate::query::locking) fn collect_source_leaves<'a, S: Clone + Send + Sync + 'static>(
    source: &'a SourcePlan,
    nullable: bool,
    catalog: &'a CatalogReadView,
    resolution: &RelationNameResolution,
    ctes: &CteScope<S>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<LockLeaf<'a>>>, SQLError> {
    let mut collector = LeafCollector {
        catalog,
        resolution,
        ctes,
        output: ProductionVec::new(*control),
        control: *control,
    };
    collector.visit(source, nullable)?;
    Ok(collector.output.finish()?)
}

impl<'a, S: Clone + Send + Sync + 'static> LeafCollector<'a, '_, S> {
    fn push(&mut self, leaf: LockLeaf<'a>) -> Result<(), SQLError> {
        self.output.push_produced(
            self.control
                .finish(leaf, self.control.empty_reservation())?,
        )?;
        Ok(())
    }

    fn visit(&mut self, source: &'a SourcePlan, nullable: bool) -> Result<(), SQLError> {
        self.control.check()?;
        let mut leaf = LockLeaf {
            names: [None; 3],
            qualifier: "",
            storage_name: self.control.copy_text("")?,
            display_name: "retained rows",
            kind: LockLeafKind::Values,
            nullable,
        };
        match source {
            SourcePlan::RetainedRows(_) => {}
            SourcePlan::Table {
                name,
                qualifier,
                alias,
                include_descendants,
                ..
            } => {
                return self.table(
                    name,
                    alias.as_deref().unwrap_or(qualifier),
                    alias.is_some(),
                    *include_descendants,
                    nullable,
                );
            }
            SourcePlan::Join {
                left, right, kind, ..
            } => {
                let (left_nullable, right_nullable) = super::join_nullability(*kind, nullable);
                self.visit(left, left_nullable)?;
                return self.visit(right, right_nullable);
            }
            SourcePlan::Values { alias, .. } => {
                leaf.names[0] = alias.as_deref();
                leaf.qualifier = alias.as_deref().unwrap_or("");
                leaf.display_name = alias.as_deref().unwrap_or("values");
            }
            SourcePlan::Function {
                name,
                output_name,
                alias,
                ..
            } => {
                leaf.qualifier = alias.as_deref().unwrap_or(output_name);
                leaf.names = [Some(leaf.qualifier), Some(output_name), Some(name)];
                leaf.display_name = leaf.qualifier;
                leaf.kind = LockLeafKind::Function;
            }
            SourcePlan::FunctionGroup {
                functions, alias, ..
            } => {
                let first = functions
                    .first()
                    .ok_or_else(|| SQLError::Internal("ROWS FROM group has no functions".into()))?;
                leaf.qualifier = alias.as_deref().unwrap_or(&first.output_name);
                leaf.names[0] = Some(leaf.qualifier);
                leaf.display_name = leaf.qualifier;
                leaf.kind = LockLeafKind::Function;
            }
            SourcePlan::Subquery { body, alias, .. } => {
                leaf.qualifier = alias.as_deref().unwrap_or("");
                leaf.names[0] = (!leaf.qualifier.is_empty()).then_some(leaf.qualifier);
                leaf.display_name = if leaf.qualifier.is_empty() {
                    "subquery"
                } else {
                    leaf.qualifier
                };
                leaf.kind = LockLeafKind::Subquery(body);
            }
        }
        self.push(leaf)
    }

    fn table(
        &mut self,
        name: &'a str,
        visible: &'a str,
        aliased: bool,
        descendants: bool,
        nullable: bool,
    ) -> Result<(), SQLError> {
        let names = [
            Some(visible),
            (!aliased && !name.is_empty()).then_some(name),
            (!aliased)
                .then(|| {
                    name.rsplit_once('.')
                        .map(|(_, local)| local)
                        .filter(|local| !local.is_empty())
                })
                .flatten(),
        ];
        let kind = self.classify(name)?;
        if matches!(kind, LockLeafKind::Base) {
            let tables = self.catalog.hierarchy_scan_tables_with_control(
                self.resolution,
                name,
                descendants,
                &self.control,
            )?;
            for storage in tables.iter() {
                self.push(LockLeaf {
                    names,
                    qualifier: visible,
                    storage_name: self.control.copy_text(storage)?,
                    display_name: visible,
                    kind: LockLeafKind::Base,
                    nullable,
                })?;
            }
            return Ok(());
        }
        self.push(LockLeaf {
            names,
            qualifier: visible,
            storage_name: self.control.copy_text(name)?,
            display_name: visible,
            kind,
            nullable,
        })
    }

    fn classify(&self, name: &str) -> Result<LockLeafKind<'a>, SQLError> {
        if self.ctes.is_visible_cte(name) {
            return Ok(LockLeafKind::Cte);
        }
        if let Some(view) =
            self.catalog
                .view_resolved_with_control(self.resolution, name, &self.control)?
        {
            return Ok(LockLeafKind::View(&view.query));
        }
        if self
            .catalog
            .foreign_table_resolved_with_control(self.resolution, name, &self.control)?
            .is_some()
        {
            return Ok(LockLeafKind::Foreign);
        }
        if self
            .catalog
            .table_resolved_with_control(self.resolution, name, &self.control)?
            .is_some()
        {
            return Ok(LockLeafKind::Base);
        }
        let lockable = self
            .catalog
            .virtual_relation_resolved_with_control(self.resolution, name, &self.control)?
            .is_some_and(uqa_sql::catalog::VirtualRelation::accepts_row_lock);
        Ok(LockLeafKind::Virtual { lockable })
    }
}
