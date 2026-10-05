//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! What a stored query references, as `find_expr_references_walker` finds it in the analyzed query: each relation in a range table, the relation columns that expressions and join conditions name, the types of constants and coercions, bound routines, and the objects that `reg*` constants name.

use uqa_core::Value;
use uqa_sql::ast::FunctionBinding;
use uqa_sql::ir::ScalarExpr;
use uqa_sql::plan::{QueryBlockPlan, QueryPlan, RelationalPlan, SourcePlan};

use super::{query_columns, Column, Deparser, RelationLookupMode, SQLError, Scope};
use crate::catalog::{CatalogReadView, RelationNameResolution};

/// The references of one stored query, relations and columns named by their bound relation names.
#[derive(Debug, Clone, Default)]
pub struct QueryReferences {
    pub relations: Vec<String>,
    /// Relation columns by relation name and column name.
    pub columns: Vec<(String, String)>,
    pub types: Vec<String>,
    pub routines: Vec<FunctionBinding>,
    /// `reg*` constants by type name and OID.
    pub constants: Vec<(String, i64)>,
}

pub fn query_references(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    query: &QueryPlan,
) -> Result<QueryReferences, SQLError> {
    let mut dynamic = resolution.clone();
    dynamic.set_lookup_mode(RelationLookupMode::Dynamic);
    let mut bound = resolution.clone();
    bound.set_lookup_mode(RelationLookupMode::Bound);
    let deparser = Deparser {
        catalog,
        dynamic,
        bound,
        pretty: false,
        wrap: 0,
        standalone: false,
        indent: true,
        routine: None,
        aliases: std::cell::OnceCell::new(),
    };
    let mut references = QueryReferences::default();
    deparser.reference_query(query, &Scope::default(), &mut references)?;
    Ok(references)
}

impl Deparser<'_> {
    fn reference_query(
        &self,
        query: &QueryPlan,
        parent: &Scope,
        references: &mut QueryReferences,
    ) -> Result<(), SQLError> {
        let mut scope = parent.clone();
        for cte in &query.ctes {
            let mut names = query_columns(super::query::view_cte_query(cte)?);
            for (name, alias) in names.iter_mut().zip(&cte.columns) {
                name.clone_from(alias);
            }
            scope.ctes.insert(cte.name.clone(), names);
        }
        for cte in &query.ctes {
            self.reference_query(
                super::query::view_cte_query(cte)?,
                &scope.child(),
                references,
            )?;
        }
        match &query.root {
            RelationalPlan::QueryBlock(block) => self.reference_block(block, &scope, references),
            RelationalPlan::SetOp {
                left,
                right,
                subqueries,
                ..
            } => {
                self.reference_query(left, &scope, references)?;
                self.reference_query(right, &scope, references)?;
                for subquery in subqueries {
                    self.reference_query(subquery, &scope.child(), references)?;
                }
                Ok(())
            }
            RelationalPlan::Values { rows, subqueries } => {
                for expression in rows.iter().flatten() {
                    reference_expression(expression, &scope, references);
                }
                for subquery in subqueries {
                    self.reference_query(subquery, &scope.child(), references)?;
                }
                Ok(())
            }
        }
    }

    fn reference_block(
        &self,
        block: &QueryBlockPlan,
        parent: &Scope,
        references: &mut QueryReferences,
    ) -> Result<(), SQLError> {
        let mut scope = parent.clone();
        scope.columns = block
            .from
            .as_ref()
            .map(|source| self.source_columns(source, parent))
            .transpose()?
            .unwrap_or_default();
        for projection in &block.projections {
            reference_expression(&projection.expr, &scope, references);
        }
        for expression in block
            .r#where
            .iter()
            .chain(&block.having)
            .chain(&block.limit)
            .chain(&block.offset)
            .chain(&block.group_by)
            .chain(&block.distinct_on)
            .chain(block.grouping_sets.iter().flatten())
        {
            reference_expression(expression, &scope, references);
        }
        for order in &block.order_by {
            // An output column named by its alias was referenced by its projection.
            let is_output = matches!(&order.expr, ScalarExpr::Column(name) if block.projections.iter().any(|projection| projection.alias.as_ref() == Some(name)));
            if !is_output {
                reference_expression(&order.expr, &scope, references);
            }
        }
        if let Some(source) = &block.from {
            self.reference_source(source, &scope, references)?;
        }
        for subquery in &block.subqueries {
            self.reference_query(subquery, &scope.child(), references)?;
        }
        Ok(())
    }

    fn reference_source(
        &self,
        source: &SourcePlan,
        scope: &Scope,
        references: &mut QueryReferences,
    ) -> Result<(), SQLError> {
        match source {
            SourcePlan::Table { name, .. } => {
                if super::sources::cte_source_columns(scope, name).is_none() {
                    references.relations.push(name.clone());
                }
            }
            SourcePlan::Subquery { body, .. } => {
                self.reference_query(body, &scope.child(), references)?;
            }
            SourcePlan::Values { rows, .. } => {
                for expression in rows.iter().flatten() {
                    reference_expression(expression, scope, references);
                }
            }
            SourcePlan::Function {
                binding,
                args,
                relations,
                ..
            } => {
                references.routines.extend(binding.iter().cloned());
                // Operators such as `vector_similarity_join` read the relations they name.
                if let Some(relations) = relations {
                    references.relations.push(relations.left.clone());
                    references.relations.push(relations.right.clone());
                }
                for expression in args {
                    reference_expression(expression, scope, references);
                }
            }
            SourcePlan::FunctionGroup { functions, .. } => {
                for function in functions {
                    references.routines.extend(function.binding.iter().cloned());
                    if let Some(relations) = &function.relations {
                        references.relations.push(relations.left.clone());
                        references.relations.push(relations.right.clone());
                    }
                    for expression in &function.args {
                        reference_expression(expression, scope, references);
                    }
                }
            }
            SourcePlan::Join {
                left,
                right,
                on,
                using,
                natural,
                ..
            } => {
                // The condition of a `USING` or `NATURAL` join compares each merged column of the two inputs.
                let left_columns = self.source_columns(left, scope)?;
                let right_columns = self.source_columns(right, scope)?;
                let merged = using.as_ref().map_or_else(
                    || {
                        if *natural {
                            left_columns
                                .iter()
                                .filter(|column| {
                                    right_columns.iter().any(|other| other.name == column.name)
                                })
                                .map(|column| column.name.clone())
                                .collect()
                        } else {
                            Vec::new()
                        }
                    },
                    |using| using.columns.clone(),
                );
                for name in &merged {
                    for columns in [&left_columns, &right_columns] {
                        if let Some(column) = columns.iter().find(|column| column.name == *name) {
                            reference_column(column, references);
                        }
                    }
                }
                if let Some(on) = on {
                    reference_expression(on, scope, references);
                }
                self.reference_source(left, scope, references)?;
                self.reference_source(right, scope, references)?;
            }
        }
        Ok(())
    }
}

fn reference_column(column: &Column, references: &mut QueryReferences) {
    if let Some((relation, name)) = &column.base {
        references.columns.push((relation.clone(), name.clone()));
    }
}

fn reference_expression(expression: &ScalarExpr, scope: &Scope, references: &mut QueryReferences) {
    expression.visit(&mut |node| match node {
        ScalarExpr::Column(name) => {
            if let Some(column) = scope_column(scope, None, name) {
                reference_column(column, references);
            }
        }
        ScalarExpr::QualifiedColumn { qualifier, column } => {
            if let Some(column) = scope_column(scope, Some(qualifier), column) {
                reference_column(column, references);
            }
        }
        // A star names every column of its sources.
        ScalarExpr::Star => {
            for column in &scope.columns {
                reference_column(column, references);
            }
        }
        ScalarExpr::QualifiedStar(qualifier) => {
            for column in scope
                .columns
                .iter()
                .chain(&scope.outer)
                .filter(|column| column.qualifier == *qualifier)
            {
                reference_column(column, references);
            }
        }
        ScalarExpr::TypedLiteral { value, ty, .. } => {
            references.types.push(ty.clone());
            if let Value::Int(oid) = value {
                references.constants.push((ty.clone(), *oid));
            }
        }
        ScalarExpr::Cast { ty, .. } => references.types.push(ty.clone()),
        ScalarExpr::Func {
            name,
            binding,
            args,
            ..
        } => {
            references.routines.extend(binding.iter().cloned());
            if let Some(sequence) = sequence_argument(name, args) {
                references.relations.push(sequence.to_string());
            }
        }
        _ => {}
    });
}

/// The sequence a sequence function names by a constant, which analysis turns into a `regclass` constant.
fn sequence_argument<'a>(name: &str, args: &'a [ScalarExpr]) -> Option<&'a str> {
    let lower = name.to_ascii_lowercase();
    let local = lower.strip_prefix("pg_catalog.").unwrap_or(&lower);
    if !matches!(local, "nextval" | "currval" | "setval") {
        return None;
    }
    let mut argument = args.first()?;
    while let ScalarExpr::Cast { expr, ty } = argument {
        if !ty.eq_ignore_ascii_case("regclass") && !ty.eq_ignore_ascii_case("pg_catalog.regclass") {
            return None;
        }
        argument = expr;
    }
    match argument {
        ScalarExpr::Literal(Value::Str(sequence)) => Some(sequence),
        _ => None,
    }
}

fn scope_column<'a>(scope: &'a Scope, qualifier: Option<&str>, name: &str) -> Option<&'a Column> {
    scope.columns.iter().chain(&scope.outer).find(|column| {
        column.name == name && qualifier.is_none_or(|qualifier| qualifier == column.qualifier)
    })
}
