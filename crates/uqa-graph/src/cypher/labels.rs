//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Default-label requirements and diagnostics for Cypher queries.

/// Check required AGE default label relations using the caller's selected graph handle.
pub fn validate_default_label_relations(
    store: &crate::GraphStoreHandle,
    graph: &str,
    query: &super::CypherQuery,
) -> Result<(), super::CypherError> {
    use super::CypherError;
    let labels = store
        .graph_labels(graph)
        .map_err(|error| CypherError::Storage(error.to_string()))?;
    let (requires_vertex, requires_edge) = label_requirements(query);
    for (required, kind) in [
        (requires_vertex, crate::LabelKind::Vertex),
        (requires_edge, crate::LabelKind::Edge),
    ] {
        if required
            && !labels
                .iter()
                .any(|label| label.id == kind.default_label_id())
        {
            return Err(CypherError::MissingLabelRelation(format!(
                "{graph}.{}",
                kind.default_label_name()
            )));
        }
    }
    Ok(())
}

fn mark_path_requirements(path: &super::PathPattern, required: &mut (bool, bool)) {
    use super::PathElement;

    for element in &path.elements {
        match element {
            PathElement::Node(node) => {
                required.0 = true;
                if let Some(properties) = &node.properties {
                    for expression in properties.values() {
                        mark_expression_requirements(expression, required);
                    }
                }
            }
            PathElement::Rel(relation) => {
                required.1 = true;
                if let Some(properties) = &relation.properties {
                    for expression in properties.values() {
                        mark_expression_requirements(expression, required);
                    }
                }
            }
        }
    }
}

fn mark_expression_requirements(expression: &super::CypherExpr, required: &mut (bool, bool)) {
    use super::CypherExpr;

    match expression {
        CypherExpr::FunctionCall(call) => {
            for argument in &call.args {
                mark_expression_requirements(argument, required);
            }
        }
        CypherExpr::BinaryOp(binary) => {
            mark_expression_requirements(&binary.left, required);
            mark_expression_requirements(&binary.right, required);
        }
        CypherExpr::UnaryOp(unary) => {
            mark_expression_requirements(&unary.operand, required);
        }
        CypherExpr::ListIndex(index) => {
            mark_expression_requirements(&index.expr, required);
            mark_expression_requirements(&index.index, required);
        }
        CypherExpr::ListSlice(slice) => {
            mark_expression_requirements(&slice.expr, required);
            if let Some(start) = &slice.start {
                mark_expression_requirements(start, required);
            }
            if let Some(end) = &slice.end {
                mark_expression_requirements(end, required);
            }
        }
        CypherExpr::ListComprehension(comprehension) => {
            mark_expression_requirements(&comprehension.list_expr, required);
            if let Some(filter) = &comprehension.filter {
                mark_expression_requirements(filter, required);
            }
            if let Some(map) = &comprehension.map_expr {
                mark_expression_requirements(map, required);
            }
        }
        CypherExpr::InList(list) => {
            mark_expression_requirements(&list.expr, required);
            mark_expression_requirements(&list.list_expr, required);
        }
        CypherExpr::IsNull(null) => mark_expression_requirements(&null.expr, required),
        CypherExpr::IsNotNull(not_null) => {
            mark_expression_requirements(&not_null.expr, required);
        }
        CypherExpr::CaseExpr(case) => {
            if let Some(operand) = &case.operand {
                mark_expression_requirements(operand, required);
            }
            for (condition, result) in &case.whens {
                mark_expression_requirements(condition, required);
                mark_expression_requirements(result, required);
            }
            if let Some(else_expression) = &case.else_expr {
                mark_expression_requirements(else_expression, required);
            }
        }
        CypherExpr::ListLiteral(list) => {
            for element in &list.elements {
                mark_expression_requirements(element, required);
            }
        }
        CypherExpr::MapLiteral(map) => {
            for (_, value) in &map.pairs {
                mark_expression_requirements(value, required);
            }
        }
        CypherExpr::ExistsPattern(path) => mark_path_requirements(path, required),
        CypherExpr::PropertyAccess(_)
        | CypherExpr::Parameter(_)
        | CypherExpr::Literal(_)
        | CypherExpr::Variable(_) => {}
    }
}

fn mark_return_requirements(
    items: &[super::ReturnItem],
    order_by: Option<&[super::OrderByItem]>,
    skip: Option<&super::CypherExpr>,
    limit: Option<&super::CypherExpr>,
    required: &mut (bool, bool),
) {
    for item in items {
        mark_expression_requirements(&item.expr, required);
    }
    for item in order_by.into_iter().flatten() {
        mark_expression_requirements(&item.expr, required);
    }
    if let Some(skip) = skip {
        mark_expression_requirements(skip, required);
    }
    if let Some(limit) = limit {
        mark_expression_requirements(limit, required);
    }
}

fn label_requirements(query: &super::CypherQuery) -> (bool, bool) {
    use super::CypherClause;

    let mut required = (false, false);
    for clause in &query.clauses {
        match clause {
            CypherClause::Match(clause) => {
                for path in &clause.patterns {
                    mark_path_requirements(path, &mut required);
                }
                if let Some(filter) = &clause.r#where {
                    mark_expression_requirements(filter, &mut required);
                }
            }
            CypherClause::Create(clause) => {
                for path in &clause.patterns {
                    mark_path_requirements(path, &mut required);
                }
            }
            CypherClause::Merge(clause) => {
                mark_path_requirements(&clause.pattern, &mut required);
                for item in clause
                    .on_create_set
                    .iter()
                    .chain(&clause.on_match_set)
                    .flatten()
                {
                    mark_expression_requirements(&item.target, &mut required);
                    mark_expression_requirements(&item.value, &mut required);
                }
            }
            CypherClause::Set(clause) => {
                for item in &clause.items {
                    mark_expression_requirements(&item.target, &mut required);
                    mark_expression_requirements(&item.value, &mut required);
                }
            }
            CypherClause::Delete(clause) => {
                for expression in &clause.expressions {
                    mark_expression_requirements(expression, &mut required);
                }
            }
            CypherClause::Return(clause) => mark_return_requirements(
                &clause.items,
                clause.order_by.as_deref(),
                clause.skip.as_ref(),
                clause.limit.as_ref(),
                &mut required,
            ),
            CypherClause::With(clause) => {
                mark_return_requirements(
                    &clause.items,
                    clause.order_by.as_deref(),
                    clause.skip.as_ref(),
                    clause.limit.as_ref(),
                    &mut required,
                );
                if let Some(filter) = &clause.r#where {
                    mark_expression_requirements(filter, &mut required);
                }
            }
            CypherClause::Unwind(clause) => {
                mark_expression_requirements(&clause.expr, &mut required);
            }
        }
    }
    required
}

#[cfg(test)]
mod tests;
