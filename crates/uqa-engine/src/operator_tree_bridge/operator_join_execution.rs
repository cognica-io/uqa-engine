//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent relation execution for SQL operator joins.

use uqa_sql::ast::OperatorJoinRelations;

use super::{
    execute_operator_tree_in_execution, first_structured_field, lower_operator_join_table_function,
    require_graph_name, require_shared_structured_field, require_shared_vector_field,
    require_text_field, require_vector_field, DriverResult, Engine, EngineDriver,
    GeneralizedPostingList, HybridJoinFields, OperatorOutput, OperatorTree, PostingList, SQLError,
    SQLParam, ScalarExpr,
};

struct OperatorJoinExecution<'a> {
    left_driver: EngineDriver<'a>,
    right_driver: EngineDriver<'a>,
}

impl<'a> OperatorJoinExecution<'a> {
    fn new(
        engine: &'a Engine,
        relations: &'a OperatorJoinRelations,
        params: &'a [SQLParam],
    ) -> Self {
        Self {
            left_driver: EngineDriver::new_in_execution(engine, &relations.left, params),
            right_driver: EngineDriver::new_in_execution(engine, &relations.right, params),
        }
    }

    fn operand(
        driver: &EngineDriver<'_>,
        tree: &OperatorTree,
        context: &str,
    ) -> DriverResult<PostingList> {
        match execute_operator_tree_in_execution(driver.engine, driver.table, driver.params, tree)?
        {
            OperatorOutput::Posting(result) => Ok(result),
            OperatorOutput::Graph(result) => Ok(result.to_posting_list()),
            OperatorOutput::Generalized(_) => Err(SQLError::TypeMismatch(format!(
                "{context} produces tuple rows and cannot be an operator join operand"
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
            Self::operand(&self.left_driver, left, left_context)?,
            Self::operand(&self.right_driver, right, right_context)?,
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

fn execute_cross_relation_operator_join(
    engine: &Engine,
    relations: &OperatorJoinRelations,
    params: &[SQLParam],
    tree: &OperatorTree,
) -> DriverResult<GeneralizedPostingList> {
    let execution = OperatorJoinExecution::new(engine, relations, params);
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

/// Execute a tuple-producing operator join exposed as a SQL table function.
pub(crate) fn execute_operator_join_table_function(
    engine: &Engine,
    name: &str,
    relations: Option<&OperatorJoinRelations>,
    args: &[ScalarExpr],
    params: &[SQLParam],
) -> DriverResult<GeneralizedPostingList> {
    let (relations, tree) =
        lower_operator_join_table_function(engine, name, relations, args, params)?;
    execute_cross_relation_operator_join(engine, &relations, params, &tree)
}
