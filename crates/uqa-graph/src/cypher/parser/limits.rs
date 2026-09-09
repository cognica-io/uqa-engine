//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bound parser recursion and constructed expression trees before stack exhaustion.

use super::{CypherExpr, ParseError, Parser, PathElement};

const MAX_EXPRESSION_DEPTH: usize = 64;

impl Parser {
    pub(super) fn with_expression_recursion<T>(
        &mut self,
        parse: impl FnOnce(&mut Self) -> Result<T, ParseError>,
    ) -> Result<T, ParseError> {
        if self.expression_recursion >= MAX_EXPRESSION_DEPTH {
            return Err(self.expression_depth_error());
        }
        self.expression_recursion += 1;
        let result = parse(self);
        self.expression_recursion -= 1;
        result
    }

    fn expression_depth_error(&self) -> ParseError {
        ParseError::ExpressionTooDeep {
            limit: MAX_EXPRESSION_DEPTH,
            position: self.peek().pos,
        }
    }

    /// Iterative operator parsing can still build a deeply recursive AST.
    /// Check each new chain node before evaluation, cloning, or dropping it
    /// can exhaust the stack. Width does not consume the nesting budget.
    pub(super) fn check_expression_depth(&self, expression: &CypherExpr) -> Result<(), ParseError> {
        let mut pending = vec![(expression, 1)];
        while let Some((expression, depth)) = pending.pop() {
            if depth > MAX_EXPRESSION_DEPTH {
                return Err(self.expression_depth_error());
            }
            let mut push = |child| pending.push((child, depth + 1));
            match expression {
                CypherExpr::PropertyAccess(_)
                | CypherExpr::Parameter(_)
                | CypherExpr::Literal(_)
                | CypherExpr::Variable(_) => {}
                CypherExpr::FunctionCall(call) => call.args.iter().for_each(push),
                CypherExpr::BinaryOp(op) => {
                    push(op.left.as_ref());
                    push(op.right.as_ref());
                }
                CypherExpr::UnaryOp(op) => push(op.operand.as_ref()),
                CypherExpr::ListIndex(index) => {
                    push(index.expr.as_ref());
                    push(index.index.as_ref());
                }
                CypherExpr::ListSlice(slice) => {
                    push(slice.expr.as_ref());
                    slice.start.as_deref().into_iter().for_each(&mut push);
                    slice.end.as_deref().into_iter().for_each(push);
                }
                CypherExpr::ListComprehension(list) => {
                    push(list.list_expr.as_ref());
                    list.filter.as_deref().into_iter().for_each(&mut push);
                    list.map_expr.as_deref().into_iter().for_each(push);
                }
                CypherExpr::InList(list) => {
                    push(list.expr.as_ref());
                    push(list.list_expr.as_ref());
                }
                CypherExpr::IsNull(test) => push(test.expr.as_ref()),
                CypherExpr::IsNotNull(test) => push(test.expr.as_ref()),
                CypherExpr::CaseExpr(case) => {
                    case.operand.as_deref().into_iter().for_each(&mut push);
                    for (condition, value) in &case.whens {
                        push(condition);
                        push(value);
                    }
                    case.else_expr.as_deref().into_iter().for_each(push);
                }
                CypherExpr::ListLiteral(list) => list.elements.iter().for_each(push),
                CypherExpr::MapLiteral(map) => {
                    map.pairs.iter().map(|(_, value)| value).for_each(push);
                }
                CypherExpr::ExistsPattern(pattern) => {
                    for element in &pattern.elements {
                        let properties = match element {
                            PathElement::Node(node) => &node.properties,
                            PathElement::Rel(rel) => &rel.properties,
                        };
                        if let Some(properties) = properties {
                            properties.values().for_each(&mut push);
                        }
                    }
                }
            }
        }
        Ok(())
    }
}
