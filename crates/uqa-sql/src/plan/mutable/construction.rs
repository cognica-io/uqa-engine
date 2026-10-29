//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Generated projections and derived sources admit containers before moving existing constructor owners.

use super::{
    Copying, MutableQueryPlan, MutableSourcePlan, OwnedProjection, QueryPlan, RelationalPlan,
    RewriteMemory,
};
use crate::{
    plan::{AccessPathPlan, ComputePlan, QueryBlockPlan, SourcePlan},
    SQLError,
};
use uqa_core::memory::{OwnedSet, Produced, ProductionControl};

impl QueryBlockPlan {
    /// Construct an independent mutable query root from a retained block, admitting every scalar and structural destination before copying. The enclosing query has no CTE or relation flags until its caller installs the root in its original scope.
    pub fn copy_for_rewrite(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<MutableQueryPlan, SQLError> {
        control.check()?;
        let Some(mut copying) = Copying::for_rewrite(*control) else {
            let mut query = empty_projection_query();
            query.root = RelationalPlan::QueryBlock(Box::new(self.clone()));
            return Ok(MutableQueryPlan {
                query,
                memory: None,
            });
        };
        let query = copying.query_block_root(self)?;
        Ok(MutableQueryPlan {
            query,
            memory: Some(copying.into_rewrite_memory()),
        })
    }
}

impl MutableQueryPlan {
    /// Detach WHERE with its independent scalar constructor. Its former field metadata remains with the query until another qualification is installed or the query is discarded.
    pub fn take_where(
        mut self,
        control: &ProductionControl<'_>,
    ) -> Result<(Self, Option<super::OwnedScalarExpr>), SQLError> {
        self.assert_owner(control);
        control.check()?;
        let RelationalPlan::QueryBlock(block) = &mut self.query.root else {
            return Err(SQLError::Internal(
                "qualification extraction requires a query block".into(),
            ));
        };
        let Some(expression) = block.r#where.take() else {
            return Ok((self, None));
        };
        let memory = self.memory.as_mut().map(|memory| {
            let memory = super::QueryScalarSlot::Where
                .memory_mut(memory)
                .take()
                .expect("qualification owner");
            let RewriteMemory::Scalar(memory) = memory else {
                unreachable!("scalar qualification owner")
            };
            memory
        });
        Ok((
            self,
            Some(super::OwnedScalarExpr::from_rewrite_parts(
                expression, memory,
            )),
        ))
    }

    pub(crate) fn into_rewrite_parts(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<(QueryPlan, Option<RewriteMemory>), SQLError> {
        self.assert_owner(control);
        control.check()?;
        Ok((self.query, self.memory))
    }

    /// Build a projection query from independently admitted scalar/name inputs. The query starts without FROM; callers attach a unit or relational source through the same owning boundary.
    pub fn projected(
        projections: impl ExactSizeIterator<Item = OwnedProjection>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        let query = if let Some(mut copying) = Copying::for_rewrite(*control) {
            let query = copying.projected_query()?;
            Self {
                query,
                memory: Some(copying.into_rewrite_memory()),
            }
        } else {
            let mut query = empty_projection_query();
            query.root = RelationalPlan::QueryBlock(Box::new(empty_projection_block()));
            Self {
                query,
                memory: None,
            }
        };
        query.replace_projections(projections, control)
    }

    /// Replace an optional qualification, transferring the selected scalar owner even when the query previously had no WHERE clause.
    pub fn replace_where(
        mut self,
        expression: Option<super::OwnedScalarExpr>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let RelationalPlan::QueryBlock(block) = &mut self.query.root else {
            return Err(SQLError::Internal(
                "qualification replacement requires a query block".into(),
            ));
        };
        let (expression, replacement) = match Copying::for_rewrite(*control) {
            Some(mut copying) => {
                let expression = expression
                    .map(|expression| copying.take_scalar(expression))
                    .transpose()?;
                (expression, Some(copying.into_rewrite_memory()))
            }
            None => (
                expression
                    .map(|expression| {
                        expression.finish(control).map(|expression| {
                            expression
                                .into_uncontrolled()
                                .expect("ordinary qualification")
                        })
                    })
                    .transpose()?,
                None,
            ),
        };
        block.r#where = expression;
        if let Some(memory) = &mut self.memory {
            *memory
                .field_mut("root")
                .as_mut()
                .expect("query root")
                .field_mut("block")
                .as_mut()
                .expect("query block")
                .field_mut("where") = replacement;
        }
        Ok(self)
    }

    /// Move FROM out of the query while retaining every sibling field and its exact owner.
    pub fn take_source(
        mut self,
        control: &ProductionControl<'_>,
    ) -> Result<(Self, Option<MutableSourcePlan>), SQLError> {
        self.assert_owner(control);
        control.check()?;
        let RelationalPlan::QueryBlock(block) = &mut self.query.root else {
            return Err(SQLError::Internal(
                "source extraction requires a query block".into(),
            ));
        };
        let Some(source) = block.from.take() else {
            return Ok((self, None));
        };
        let memory = self
            .memory
            .as_mut()
            .and_then(|memory| super::sources::query_source_memory(memory).take());
        Ok((self, Some(MutableSourcePlan { source, memory })))
    }

    /// Replace FROM after admitting its new field records. The old source is destroyed before its constructor leases are retired.
    pub fn replace_source(
        mut self,
        source: Option<MutableSourcePlan>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let RelationalPlan::QueryBlock(block) = &mut self.query.root else {
            return Err(SQLError::Internal(
                "source replacement requires a query block".into(),
            ));
        };
        let (source, replacement) = match Copying::for_rewrite(*control) {
            Some(mut copying) => {
                let source = source
                    .map(|source| copying.take_source(source))
                    .transpose()?;
                (source, Some(copying.into_rewrite_memory()))
            }
            None => (
                source
                    .map(|source| source.into_rewrite_parts(control).map(|parts| parts.0))
                    .transpose()?,
                None,
            ),
        };
        block.from = source;
        if let Some(memory) = &mut self.memory {
            *memory
                .field_mut("root")
                .as_mut()
                .expect("query root")
                .field_mut("block")
                .as_mut()
                .expect("query block")
                .field_mut("from") = replacement;
        }
        Ok(self)
    }
}

impl SourcePlan {
    pub fn copy_for_rewrite(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<MutableSourcePlan, SQLError> {
        control.check()?;
        let Some(mut copying) = Copying::for_rewrite(*control) else {
            return Ok(MutableSourcePlan {
                source: self.clone(),
                memory: None,
            });
        };
        let source = copying.source_fields(self)?;
        let output = MutableSourcePlan {
            source,
            memory: Some(copying.into_rewrite_memory()),
        };
        control.check()?;
        Ok(output)
    }
}

impl MutableSourcePlan {
    pub(crate) fn into_rewrite_parts(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<(SourcePlan, Option<RewriteMemory>), SQLError> {
        self.assert_owner(control);
        control.check()?;
        Ok((self.source, self.memory))
    }

    /// Construct the one-row zero-column relation used by a scalar projection.
    pub fn unit(control: &ProductionControl<'_>) -> Result<Self, SQLError> {
        control.check()?;
        let Some(mut copying) = Copying::for_rewrite(*control) else {
            return Ok(Self {
                source: SourcePlan::Values {
                    lateral: false,
                    rows: vec![Vec::new()],
                    alias: None,
                    column_aliases: Vec::new(),
                    internal_relation: None,
                    internal_column_types: Vec::new(),
                    internal_column_collations: Vec::new(),
                },
                memory: None,
            });
        };
        let source = copying.unit_source()?;
        Ok(Self {
            source,
            memory: Some(copying.into_rewrite_memory()),
        })
    }

    /// Attach a query beneath a newly admitted source box, transferring the query's own constructor records unchanged.
    pub fn subquery(
        query: MutableQueryPlan,
        alias: Option<Produced<String>>,
        columns: Produced<Vec<String>>,
        captured_output: Option<crate::ast::InternalRelationId>,
        lateral: bool,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        if let Some(mut copying) = Copying::for_rewrite(*control) {
            let source =
                copying.subquery_source(query, alias, columns, captured_output, lateral)?;
            return Ok(Self {
                source,
                memory: Some(copying.into_rewrite_memory()),
            });
        }
        let (query, _) = query.into_rewrite_parts(control)?;
        Ok(Self {
            source: SourcePlan::Subquery {
                captured_output,
                lateral,
                body: Box::new(query),
                alias: alias.map(|alias| alias.into_uncontrolled().expect("ordinary source alias")),
                column_aliases: columns
                    .into_uncontrolled()
                    .expect("ordinary source columns"),
            },
            memory: None,
        })
    }
}

pub(crate) fn empty_projection_query() -> QueryPlan {
    super::queries::empty_query()
}

pub(crate) fn empty_projection_block() -> QueryBlockPlan {
    QueryBlockPlan {
        deferred_outer_rewrites: Vec::new(),
        windows: Vec::new(),
        projections: Vec::new(),
        privilege_columns: OwnedSet::new(),
        from: None,
        r#where: None,
        compute: ComputePlan::Project,
        group_by: Vec::new(),
        grouping_sets: Vec::new(),
        group_distinct: false,
        having: None,
        order_by: Vec::new(),
        limit: None,
        with_ties: false,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        subqueries: Vec::new(),
        access: AccessPathPlan::Row,
        locking: Vec::new(),
    }
}
