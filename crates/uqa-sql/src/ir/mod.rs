//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Serializable scalar SQL IR shared by analysis, planning, and execution.

mod call_arguments;
mod traversal;
pub use call_arguments::{
    analyze_expression_call_arguments, scalar_call_argument, scalar_call_arguments,
    scalar_call_arguments_with_control, validate_scalar_call_arguments, ScalarCallArgument,
};

use crate::ast::{
    BinaryOp, ColumnType, FrameExclusion, FrameMode, FunctionBinding, FunctionOrderSyntax,
    InternalColumnRef, NullsOrder, WindowCallModifiers,
};
use uqa_core::Value;

/// Index into the query children owned by the enclosing expression plan.
pub type SubqueryId = usize;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ScalarExpr {
    Star,
    QualifiedStar(String),
    Default,
    Column(String),
    /// Logical position in an already-bound physical row schema. This variant is introduced only after relational binding so duplicate SQL labels remain independently addressable.
    Position(usize),
    /// Structural executor-only attribute, resolved independently of SQL relation and column names.
    InternalColumn(InternalColumnRef),
    QualifiedColumn {
        qualifier: String,
        column: String,
    },
    Literal(Value),
    /// An already-coerced runtime datum whose declared type must survive lowering.
    TypedLiteral {
        value: Value,
        ty: String,
        /// Resolved identity of an already-bound datum, including domain OIDs and type modifiers.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bound_type: Option<ColumnType>,
        /// Original SQL parameter slot when specialization replaces a bare parameter.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parameter_index: Option<usize>,
    },
    Param(usize),
    Func {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        binding: Option<FunctionBinding>,
        args: Vec<Self>,
        distinct: bool,
        order_by: Vec<ScalarOrder>,
        #[serde(default, skip_serializing_if = "FunctionOrderSyntax::is_legacy")]
        order_syntax: FunctionOrderSyntax,
        filter: Option<Box<Self>>,
    },
    Array(Vec<Self>),
    Row(Vec<Self>),
    CompositeRow {
        items: Vec<Self>,
        binding: crate::ast::CompositeRowBinding,
        /// Resolved result identity for physical inference without catalog callbacks.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bound_type: Option<ColumnType>,
    },
    Binary {
        op: BinaryOp,
        lhs: Box<Self>,
        rhs: Box<Self>,
    },
    UnaryMinus(Box<Self>),
    Not(Box<Self>),
    And(Vec<Self>),
    Or(Vec<Self>),
    IsNull {
        expr: Box<Self>,
        negated: bool,
    },
    Between {
        expr: Box<Self>,
        low: Box<Self>,
        high: Box<Self>,
    },
    InList {
        expr: Box<Self>,
        list: Vec<Self>,
        negated: bool,
    },
    /// A window function call; `filter` is an aggregate's `FILTER (WHERE ...)` condition.
    WindowCall {
        name: String,
        args: Vec<Self>,
        spec: ScalarWindowSpec,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filter: Option<Box<Self>>,
        #[serde(default, skip_serializing_if = "WindowCallModifiers::is_empty")]
        modifiers: WindowCallModifiers,
    },
    Case {
        base: Option<Box<Self>>,
        when: Vec<(Self, Self)>,
        else_branch: Option<Box<Self>>,
    },
    Cast {
        /// Analysis introduced this coercion rather than retaining an explicit SQL cast. Stored definitions must distinguish array coercions from explicit constructor conversions.
        #[serde(default, skip_serializing_if = "is_false")]
        implicit: bool,
        expr: Box<Self>,
        ty: String,
    },
    ScalarSubquery(SubqueryId),
    Exists {
        subquery: SubqueryId,
        negated: bool,
    },
    InSubquery {
        expr: Box<Self>,
        subquery: SubqueryId,
        negated: bool,
    },
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ScalarOrder {
    pub expr: ScalarExpr,
    pub descending: bool,
    pub nulls: Option<NullsOrder>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ScalarWindowSpec {
    /// Canonical definition in the enclosing query block; absent in legacy inline plans.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition: Option<usize>,
    pub partition_by: Vec<ScalarExpr>,
    pub order_by: Vec<ScalarOrder>,
    pub frame: Option<ScalarWindowFrame>,
}

impl ScalarWindowSpec {
    /// Borrow each key and frame-offset root owned by this specification.
    pub fn expressions(&self) -> impl Iterator<Item = &ScalarExpr> {
        self.partition_by
            .iter()
            .chain(self.order_by.iter().map(|order| &order.expr))
            .chain(
                self.frame
                    .iter()
                    .flat_map(|frame| [&frame.start, &frame.end])
                    .filter_map(|bound| match bound {
                        ScalarFrameBound::Preceding(value) | ScalarFrameBound::Following(value) => {
                            Some(value.as_ref())
                        }
                        _ => None,
                    }),
            )
    }

    /// Mutably borrow each key and frame-offset root without visiting derived call copies.
    pub fn expressions_mut(&mut self) -> impl Iterator<Item = &mut ScalarExpr> {
        self.partition_by
            .iter_mut()
            .chain(self.order_by.iter_mut().map(|order| &mut order.expr))
            .chain(
                self.frame
                    .iter_mut()
                    .flat_map(|frame| [&mut frame.start, &mut frame.end])
                    .filter_map(|bound| match bound {
                        ScalarFrameBound::Preceding(value) | ScalarFrameBound::Following(value) => {
                            Some(value.as_mut())
                        }
                        _ => None,
                    }),
            )
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ScalarWindowFrame {
    pub mode: FrameMode,
    pub start: ScalarFrameBound,
    pub end: ScalarFrameBound,
    /// Whether the frame was written `BETWEEN start AND end`; see [`crate::ast::WindowFrame::between`].
    #[serde(default = "frame_written_between")]
    pub between: bool,
    #[serde(default, skip_serializing_if = "FrameExclusion::is_no_others")]
    pub exclusion: FrameExclusion,
}

/// Frames recorded before the spelling was kept were deparsed with `BETWEEN`.
const fn frame_written_between() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ScalarFrameBound {
    UnboundedPreceding,
    UnboundedFollowing,
    CurrentRow,
    Preceding(Box<ScalarExpr>),
    Following(Box<ScalarExpr>),
}

impl ScalarExpr {
    #[must_use]
    pub fn qualified_column(qualifier: impl Into<String>, column: impl Into<String>) -> Self {
        Self::QualifiedColumn {
            qualifier: qualifier.into(),
            column: column.into(),
        }
    }
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde skip_serializing_if requires a borrowed field"
)]
const fn is_false(value: &bool) -> bool {
    !*value
}

#[cfg(test)]
mod tests {
    use super::ScalarExpr;

    #[test]
    fn syntax_cast_origin_survives_owned_and_borrowed_lowering() {
        use crate::{ast::Expr, plan::ExpressionPlan};
        use uqa_core::{memory::MemoryBudget, CancellationToken};

        let legacy = r#"{"Cast":{"expr":{"Column":"value"},"ty":"bigint[]"}}"#;
        let explicit: Expr = serde_json::from_str(legacy).unwrap();
        assert!(matches!(
            explicit,
            Expr::Cast {
                implicit: false,
                ..
            }
        ));
        assert_eq!(serde_json::to_string(&explicit).unwrap(), legacy);
        let mut implicit = explicit;
        let Expr::Cast {
            implicit: origin, ..
        } = &mut implicit
        else {
            unreachable!()
        };
        *origin = true;
        let stored = serde_json::to_string(&implicit).unwrap();
        let restored: Expr = serde_json::from_str(&stored).unwrap();
        let expected = ExpressionPlan::lower(restored.clone()).scalar;
        assert!(matches!(expected, ScalarExpr::Cast { implicit: true, .. }));
        let budget = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let borrowed =
            ExpressionPlan::lower_column_budgeted(&restored, &budget, &token, &token).unwrap();
        assert_eq!(*borrowed, expected);
    }

    #[test]
    fn cast_origin_survives_storage_and_legacy_casts_remain_explicit() {
        let legacy = r#"{"Cast":{"expr":{"Column":"value"},"ty":"bigint[]"}}"#;
        let explicit: ScalarExpr = serde_json::from_str(legacy).unwrap();
        assert!(matches!(
            explicit,
            ScalarExpr::Cast {
                implicit: false,
                ..
            }
        ));
        assert_eq!(serde_json::to_string(&explicit).unwrap(), legacy);
        let mut implicit = explicit.clone();
        let ScalarExpr::Cast {
            implicit: origin, ..
        } = &mut implicit
        else {
            unreachable!()
        };
        *origin = true;
        let stored = serde_json::to_string(&implicit).unwrap();
        assert_eq!(
            serde_json::from_str::<ScalarExpr>(&stored).unwrap(),
            implicit
        );
        assert_ne!(implicit, explicit);
    }
}
