//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent relation planning, operand execution and tuple-producing joins.

use super::driver::introspection::{
    first_structured_field, require_graph_name, require_shared_structured_field,
    require_shared_vector_field, require_text_field, require_vector_field,
};
use super::driver::{HybridJoinFields, PhysicalRetrievalDriver};
use super::runtime::{execute_tree, TreeExecutionContext};
use super::OperatorOutput;
use uqa_core::{GeneralizedPostingList, PostingList};
use uqa_operators::OperatorTree;
use uqa_sql::{ast::OperatorJoinRelations, SQLError, SQLParam};

type DriverResult<T> = Result<T, SQLError>;

struct OperatorJoinExecution<'a> {
    context: &'a TreeExecutionContext<'a>,
    left_driver: PhysicalRetrievalDriver<'a>,
    right_driver: PhysicalRetrievalDriver<'a>,
}

impl<'a> OperatorJoinExecution<'a> {
    fn new(
        context: &'a TreeExecutionContext<'a>,
        relations: &'a OperatorJoinRelations,
        params: &'a [SQLParam],
    ) -> Self {
        Self {
            context,
            left_driver: PhysicalRetrievalDriver::new(
                context.driver,
                &relations.left,
                &relations.left,
                params,
            ),
            right_driver: PhysicalRetrievalDriver::new(
                context.driver,
                &relations.right,
                &relations.right,
                params,
            ),
        }
    }

    fn operand(
        context: &TreeExecutionContext<'_>,
        driver: &PhysicalRetrievalDriver<'_>,
        tree: &OperatorTree,
        label: &str,
    ) -> DriverResult<PostingList> {
        match execute_tree(context, driver.table, driver.table, driver.params, tree)? {
            OperatorOutput::Posting(result) => Ok(result),
            OperatorOutput::Graph(result) => Ok(result.to_posting_list()),
            OperatorOutput::Generalized(_) => Err(SQLError::TypeMismatch(format!(
                "{label} produces tuple rows and cannot be an operator join operand"
            ))),
        }
    }

    fn operands(
        &self,
        left: &OperatorTree,
        right: &OperatorTree,
        left_context: &str,
        right_context: &str,
    ) -> DriverResult<(PostingList, PostingList)> {
        Ok((
            Self::operand(self.context, &self.left_driver, left, left_context)?,
            Self::operand(self.context, &self.right_driver, right, right_context)?,
        ))
    }

    fn text_similarity(
        &self,
        left: &OperatorTree,
        right: &OperatorTree,
        threshold: f64,
    ) -> DriverResult<GeneralizedPostingList> {
        let left_field = require_text_field(left, "TextSimilarityJoin.left")?;
        let right_field = require_text_field(right, "TextSimilarityJoin.right")?;
        let (left_source, right_source) = self.operands(
            left,
            right,
            "TextSimilarityJoin.left",
            "TextSimilarityJoin.right",
        )?;
        self.left_driver.join_text_similarity_postings(
            &self.right_driver,
            &left_source,
            &left_field,
            &right_source,
            &right_field,
            threshold,
        )
    }

    fn vector_similarity(
        &self,
        left: &OperatorTree,
        right: &OperatorTree,
        threshold: f64,
    ) -> DriverResult<GeneralizedPostingList> {
        let left_field = require_vector_field(left, "VectorSimilarityJoin.left")?;
        let right_field = require_vector_field(right, "VectorSimilarityJoin.right")?;
        let (left_source, right_source) = self.operands(
            left,
            right,
            "VectorSimilarityJoin.left",
            "VectorSimilarityJoin.right",
        )?;
        self.left_driver.join_vector_similarity_postings(
            &self.right_driver,
            &left_source,
            &left_field,
            &right_source,
            &right_field,
            threshold,
        )
    }

    fn hybrid(
        &self,
        left: &OperatorTree,
        right: &OperatorTree,
    ) -> DriverResult<GeneralizedPostingList> {
        let structured = require_shared_structured_field(left, right, "HybridJoin")?;
        let vector = require_shared_vector_field(left, right, "HybridJoin")?;
        let (left_source, right_source) =
            self.operands(left, right, "HybridJoin.left", "HybridJoin.right")?;
        self.left_driver.join_hybrid_postings(
            &self.right_driver,
            &left_source,
            &right_source,
            HybridJoinFields {
                left_structured: &structured.0,
                left_vector: &vector.0,
                right_structured: &structured.1,
                right_vector: &vector.1,
            },
        )
    }

    fn graph(
        &self,
        left: &OperatorTree,
        right: &OperatorTree,
        label: Option<&str>,
        graph: &str,
    ) -> DriverResult<GeneralizedPostingList> {
        let (left, right) = self.operands(left, right, "GraphJoin.left", "GraphJoin.right")?;
        self.left_driver
            .join_graph_postings(&left, &right, label, graph)
    }

    fn cross_paradigm(
        &self,
        left: &OperatorTree,
        right: &OperatorTree,
    ) -> DriverResult<GeneralizedPostingList> {
        let graph = require_graph_name(left, "CrossParadigmJoin.left")?;
        let vertex_field = first_structured_field(left)
            .or_else(|| first_structured_field(right))
            .ok_or_else(|| {
                SQLError::TypeMismatch(
                    "CrossParadigmJoin operands do not identify a join property".into(),
                )
            })?;
        let document_field = first_structured_field(right).unwrap_or_else(|| vertex_field.clone());
        let (left, right) = self.operands(
            left,
            right,
            "CrossParadigmJoin.left",
            "CrossParadigmJoin.right",
        )?;
        self.left_driver.join_cross_paradigm_postings(
            &self.right_driver,
            &left,
            &right,
            &graph,
            &vertex_field,
            &document_field,
        )
    }
}

pub fn execute_cross_relation_operator_join(
    context: &TreeExecutionContext<'_>,
    relations: &OperatorJoinRelations,
    params: &[SQLParam],
    tree: &OperatorTree,
) -> DriverResult<GeneralizedPostingList> {
    let execution = OperatorJoinExecution::new(context, relations, params);
    match tree {
        OperatorTree::TextSimilarityJoin {
            left,
            right,
            threshold,
        } => execution.text_similarity(left, right, *threshold),
        OperatorTree::VectorSimilarityJoin {
            left,
            right,
            threshold,
        } => execution.vector_similarity(left, right, *threshold),
        OperatorTree::HybridJoin { left, right } => execution.hybrid(left, right),
        OperatorTree::GraphJoin {
            left,
            right,
            label,
            graph,
        } => execution.graph(left, right, label.as_deref(), graph),
        OperatorTree::CrossParadigmJoin { left, right } => execution.cross_paradigm(left, right),
        _ => Err(SQLError::Internal(
            "operator join table function lowered to a non-join root".into(),
        )),
    }
}
