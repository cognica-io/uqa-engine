//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Apply a descriptor change to typed catalog datums without re-reading input text or repeating domain constraints.

use super::{
    apply_attribute_change, type_contains_composite, AttributeChange, CompositeTypeCatalog,
};
use crate::{
    ast::{
        ColumnDef, ColumnType, Expr, PartitionBound, PartitionRangeDatum, PartitionSpec, Statement,
        TableCheck,
    },
    plan::{QueryPlan, UnifiedPlan},
    type_resolution::FunctionTypeResolver,
    SQLError, ScalarExpr,
};
use uqa_core::Value;

pub struct CompositeConstantChange<'a> {
    pub target: u32,
    pub change: &'a AttributeChange,
    pub catalog: &'a dyn CompositeTypeCatalog,
    pub types: &'a dyn FunctionTypeResolver,
    pub rename: Option<&'a crate::binding::composite_rename::CompositeFieldRename<'a>>,
}

impl CompositeConstantChange<'_> {
    pub fn value(&self, value: &mut Value, ty: &ColumnType) -> Result<bool, SQLError> {
        if matches!(value, Value::Null) || !type_contains_composite(ty, self.target, self.catalog)?
        {
            return Ok(false);
        }
        *value = apply_attribute_change(
            std::mem::replace(value, Value::Null),
            ty,
            self.target,
            self.change,
            self.catalog,
        )?;
        Ok(true)
    }

    fn typed_value(
        &self,
        value: &mut Value,
        name: &str,
        source: &mut Option<Box<super::CompositeConstantSource>>,
    ) -> Result<bool, SQLError> {
        let Some(ty) = self.types.resolve_type_name(name)? else {
            return if crate::ast::UserTypeIdentity::parse(name).is_some() {
                Err(SQLError::Internal(format!(
                    "stored constant type {name} disappeared"
                )))
            } else {
                Ok(false)
            };
        };
        if !matches!(value, Value::Null) && type_contains_composite(&ty, self.target, self.catalog)?
        {
            if source.is_none() {
                *source = Some(Box::new(super::CompositeConstantSource::capture(
                    value,
                    &ty,
                    Some(self.catalog),
                    self.types.enum_labels(),
                )?));
            }
            source
                .as_mut()
                .expect("captured composite source")
                .retain_enum_oids(self.types.enum_labels())?;
            if let AttributeChange::Type { name, to, .. } = self.change {
                *value = source
                    .as_ref()
                    .expect("captured composite source")
                    .project_type_change(&ty, self.target, name, to, self.catalog)?;
                return Ok(true);
            }
        }
        self.value(value, &ty)
    }

    fn syntax_node(&self, node: &mut Expr) -> Result<bool, SQLError> {
        match node {
            Expr::CompositeRow { binding, .. } => {
                super::constructor::retain_argument_types(binding, self.types, Some(self.catalog))
            }
            Expr::TypedLiteral {
                value,
                ty,
                composite_source,
            } => self.typed_value(value, ty, composite_source),
            _ => Ok(false),
        }
    }

    fn scalar_node(&self, node: &mut ScalarExpr) -> Result<bool, SQLError> {
        match node {
            ScalarExpr::CompositeRow { binding, .. } => {
                super::constructor::retain_argument_types(binding, self.types, Some(self.catalog))
            }
            ScalarExpr::TypedLiteral {
                value,
                ty,
                composite_source,
                ..
            } => self.typed_value(value, ty, composite_source),
            _ => Ok(false),
        }
    }

    pub fn expression(&self, expression: &mut Expr) -> Result<bool, SQLError> {
        self.expression_in_schema(expression, &crate::RowSchema::default())
    }

    pub fn expression_in_schema(
        &self,
        expression: &mut Expr,
        schema: &crate::RowSchema,
    ) -> Result<bool, SQLError> {
        let mut changed = self
            .rename
            .map_or(Ok(false), |rename| rename.expression(expression, schema))?;
        crate::catalog::stored_ast::visit_stored_expression(expression, &mut |node| {
            changed |= self.syntax_node(node)?;
            Ok(())
        })?;
        Ok(changed)
    }

    pub fn statement(&self, statement: &mut Statement) -> Result<bool, SQLError> {
        self.statement_in_scope(statement, None, None)
    }

    pub fn statement_in_scope(
        &self,
        statement: &mut Statement,
        definition: Option<&crate::ast::CreateFunction>,
        outer: Option<&crate::RowSchema>,
    ) -> Result<bool, SQLError> {
        let mut changed = self.rename.map_or(Ok(false), |rename| {
            rename.statement(statement, definition, outer)
        })?;
        crate::catalog::stored_ast::visit_stored_statement_expressions(statement, &mut |node| {
            changed |= self.syntax_node(node)?;
            Ok(())
        })?;
        Ok(changed)
    }

    pub fn query(&self, query: &mut QueryPlan) -> Result<bool, SQLError> {
        let changed = self
            .rename
            .map_or(Ok(false), |rename| rename.query(query))?;
        self.plan_nodes(|visit| query.rewrite_scalar_expressions(visit))
            .map(|values| values || changed)
    }

    pub fn plan(&self, plan: &mut UnifiedPlan) -> Result<bool, SQLError> {
        let changed = self.rename.is_some_and(|rename| rename.bound_plan(plan));
        self.plan_nodes(|visit| plan.rewrite_scalar_expressions(visit))
            .map(|values| values || changed)
    }

    pub fn expression_plan(
        &self,
        plan: &mut crate::plan::ExpressionPlan,
    ) -> Result<bool, SQLError> {
        let mut changed = self
            .plan_nodes(|visit| crate::plan::rewrite_scalar_expression(&mut plan.scalar, visit))?;
        for query in &mut plan.subqueries {
            changed |= self.query(query)?;
        }
        Ok(changed)
    }

    fn plan_nodes(
        &self,
        visit: impl FnOnce(&mut dyn FnMut(&mut ScalarExpr)),
    ) -> Result<bool, SQLError> {
        let mut changed = false;
        let mut failure = None;
        visit(&mut |node| {
            if failure.is_some() {
                return;
            }
            match self.scalar_node(node) {
                Ok(value) => changed |= value,
                Err(error) => failure = Some(error),
            }
        });
        failure.map_or(Ok(changed), Err)
    }

    pub fn columns(&self, columns: &mut [ColumnDef]) -> Result<bool, SQLError> {
        let schema = crate::RowSchema::with_types(
            columns.iter().map(|column| column.name.clone()).collect(),
            columns
                .iter()
                .map(|column| Some(column.ty.clone()))
                .collect(),
        );
        let mut changed = false;
        for column in columns {
            for expression in column.default.iter_mut().chain(column.check.iter_mut()) {
                changed |= self.expression_in_schema(expression, &schema)?;
            }
            if let Some(generated) = &mut column.generated {
                changed |= self.expression_in_schema(&mut generated.expression, &schema)?;
            }
        }
        Ok(changed)
    }

    pub fn checks(
        &self,
        checks: &mut [TableCheck],
        columns: &[ColumnDef],
    ) -> Result<bool, SQLError> {
        let schema = crate::RowSchema::with_types(
            columns.iter().map(|column| column.name.clone()).collect(),
            columns
                .iter()
                .map(|column| Some(column.ty.clone()))
                .collect(),
        );
        let mut changed = false;
        for check in checks {
            changed |= self.expression_in_schema(&mut check.expr, &schema)?;
            if let Some(partition) = &mut check.partition_constraint {
                changed |= self.bound(&mut partition.bound, &partition.spec, columns)?;
                changed |= self.spec(&mut partition.spec)?;
            }
        }
        Ok(changed)
    }

    pub fn spec(&self, spec: &mut PartitionSpec) -> Result<bool, SQLError> {
        let mut changed = false;
        for key in &mut spec.keys {
            changed |= self.expression(key)?;
        }
        Ok(changed)
    }

    pub fn bound(
        &self,
        bound: &mut PartitionBound,
        spec: &PartitionSpec,
        columns: &[ColumnDef],
    ) -> Result<bool, SQLError> {
        let mut changed = false;
        for (position, key) in spec.keys.iter().enumerate() {
            let ty = crate::semantics::partition::partition_key_type(self.types, key, columns)?;
            match bound {
                PartitionBound::List(values) => {
                    for expression in values {
                        changed |= self.bound_value(expression, &ty)?;
                    }
                }
                PartitionBound::Range { lower, upper } => {
                    for point in [lower.get_mut(position), upper.get_mut(position)]
                        .into_iter()
                        .flatten()
                    {
                        if let PartitionRangeDatum::Value(expression) = point {
                            changed |= self.bound_value(expression, &ty)?;
                        }
                    }
                }
                PartitionBound::Default | PartitionBound::Hash { .. } => {}
            }
        }
        Ok(changed)
    }

    fn bound_value(&self, expression: &mut Expr, ty: &ColumnType) -> Result<bool, SQLError> {
        match expression {
            Expr::Literal(value) | Expr::TypedLiteral { value, .. } => self.value(value, ty),
            _ => Err(SQLError::Internal(
                "partition bound datum was not evaluated".into(),
            )),
        }
    }
}

#[cfg(test)]
mod tests;
