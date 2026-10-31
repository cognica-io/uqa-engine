//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact scalar JSON identities borrow metadata and admit both output bytes and nested datum-formatting scratch.

use crate::{SQLError, ScalarExpr};
use serde::{
    ser::{SerializeSeq, SerializeStruct, SerializeStructVariant},
    Serialize, Serializer,
};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    ValueSerialization,
};

pub(super) fn identity(
    expression: &ScalarExpr,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<u8>>, SQLError> {
    let values = ValueSerialization::new(*control);
    let mut writer = Writer {
        output: ProductionVec::new(*control),
        error: None,
    };
    let serialized = serde_json::to_writer(
        &mut writer,
        &Expression {
            expression,
            values: &values,
        },
    );
    if let Some(error) = writer.error {
        return Err(error.into());
    }
    if let Some(error) = values.take_error() {
        return Err(error.into());
    }
    serialized.map_err(|error| {
        SQLError::Internal(format!(
            "serialize GROUP BY DISTINCT expression identity: {error}"
        ))
    })?;
    Ok(writer.output.finish()?)
}

struct Writer<'a> {
    output: ProductionVec<'a, u8>,
    error: Option<uqa_core::ValueRetentionError>,
}
impl std::io::Write for Writer<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        match self.output.extend_from_slice(bytes) {
            Ok(()) => Ok(bytes.len()),
            Err(error) => {
                self.error = Some(error);
                Err(std::io::ErrorKind::Other.into())
            }
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct Expression<'a, 'c> {
    expression: &'a ScalarExpr,
    values: &'a ValueSerialization<'c>,
}
impl Expression<'_, '_> {
    fn child<'s>(&'s self, expression: &'s ScalarExpr) -> Expression<'s, 's> {
        Expression {
            expression,
            values: self.values,
        }
    }
}

impl Serialize for Expression<'_, '_> {
    #[expect(
        clippy::too_many_lines,
        reason = "exhaustive serialization retains the scalar IR field order"
    )]
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let values = self.values;
        match self.expression {
            ScalarExpr::Literal(value) => serializer.serialize_newtype_variant(
                "ScalarExpr",
                0,
                "Literal",
                &values.value(value),
            ),
            ScalarExpr::TypedLiteral {
                value,
                ty,
                collation_oid,
                composite_source,
                bound_type,
                parameter_index,
            } => {
                let mut output = serializer.serialize_struct_variant(
                    "ScalarExpr",
                    0,
                    "TypedLiteral",
                    2 + usize::from(collation_oid.is_some())
                        + usize::from(composite_source.is_some())
                        + usize::from(bound_type.is_some())
                        + usize::from(parameter_index.is_some()),
                )?;
                output.serialize_field("value", &values.value(value))?;
                output.serialize_field("ty", ty)?;
                if let Some(oid) = collation_oid {
                    output.serialize_field("collation_oid", oid)?;
                }
                if let Some(source) = composite_source {
                    output
                        .serialize_field("composite_source", &CompositeSource { source, values })?;
                }
                if let Some(ty) = bound_type {
                    output.serialize_field("bound_type", ty)?;
                }
                if let Some(index) = parameter_index {
                    output.serialize_field("parameter_index", index)?;
                }
                output.end()
            }
            ScalarExpr::Func {
                name,
                binding,
                args,
                distinct,
                order_by,
                order_syntax,
                filter,
            } => {
                let mut output = serializer.serialize_struct_variant(
                    "ScalarExpr",
                    0,
                    "Func",
                    5 + usize::from(binding.is_some()) + usize::from(!order_syntax.is_legacy()),
                )?;
                output.serialize_field("name", name)?;
                if let Some(binding) = binding {
                    output.serialize_field("binding", binding)?;
                }
                output.serialize_field(
                    "args",
                    &Expressions {
                        expressions: args,
                        values,
                    },
                )?;
                output.serialize_field("distinct", distinct)?;
                output.serialize_field(
                    "order_by",
                    &Orders {
                        orders: order_by,
                        values,
                    },
                )?;
                if !order_syntax.is_legacy() {
                    output.serialize_field("order_syntax", order_syntax)?;
                }
                output.serialize_field(
                    "filter",
                    &filter.as_deref().map(|expression| self.child(expression)),
                )?;
                output.end()
            }
            ScalarExpr::Array(items)
            | ScalarExpr::Row(items)
            | ScalarExpr::And(items)
            | ScalarExpr::Or(items) => {
                let name = match self.expression {
                    ScalarExpr::Array(_) => "Array",
                    ScalarExpr::Row(_) => "Row",
                    ScalarExpr::And(_) => "And",
                    _ => "Or",
                };
                serializer.serialize_newtype_variant(
                    "ScalarExpr",
                    0,
                    name,
                    &Expressions {
                        expressions: items,
                        values,
                    },
                )
            }
            ScalarExpr::CompositeRow {
                items,
                binding,
                bound_type,
            } => {
                let mut output = serializer.serialize_struct_variant(
                    "ScalarExpr",
                    0,
                    "CompositeRow",
                    2 + usize::from(bound_type.is_some()),
                )?;
                output.serialize_field(
                    "items",
                    &Expressions {
                        expressions: items,
                        values,
                    },
                )?;
                output.serialize_field("binding", binding)?;
                if let Some(ty) = bound_type {
                    output.serialize_field("bound_type", ty)?;
                }
                output.end()
            }
            ScalarExpr::Binary { op, lhs, rhs } => {
                let mut output =
                    serializer.serialize_struct_variant("ScalarExpr", 0, "Binary", 3)?;
                output.serialize_field("op", op)?;
                output.serialize_field("lhs", &self.child(lhs))?;
                output.serialize_field("rhs", &self.child(rhs))?;
                output.end()
            }
            ScalarExpr::UnaryMinus(inner) | ScalarExpr::Not(inner) => serializer
                .serialize_newtype_variant(
                    "ScalarExpr",
                    0,
                    if matches!(self.expression, ScalarExpr::Not(_)) {
                        "Not"
                    } else {
                        "UnaryMinus"
                    },
                    &self.child(inner),
                ),
            ScalarExpr::IsNull { expr, negated } => {
                let mut output =
                    serializer.serialize_struct_variant("ScalarExpr", 0, "IsNull", 2)?;
                output.serialize_field("expr", &self.child(expr))?;
                output.serialize_field("negated", negated)?;
                output.end()
            }
            ScalarExpr::Between { expr, low, high } => {
                let mut output =
                    serializer.serialize_struct_variant("ScalarExpr", 0, "Between", 3)?;
                output.serialize_field("expr", &self.child(expr))?;
                output.serialize_field("low", &self.child(low))?;
                output.serialize_field("high", &self.child(high))?;
                output.end()
            }
            ScalarExpr::InList {
                expr,
                list,
                negated,
            } => {
                let mut output =
                    serializer.serialize_struct_variant("ScalarExpr", 0, "InList", 3)?;
                output.serialize_field("expr", &self.child(expr))?;
                output.serialize_field(
                    "list",
                    &Expressions {
                        expressions: list,
                        values,
                    },
                )?;
                output.serialize_field("negated", negated)?;
                output.end()
            }
            ScalarExpr::WindowCall {
                name,
                args,
                spec,
                filter,
                modifiers,
            } => {
                let mut output = serializer.serialize_struct_variant(
                    "ScalarExpr",
                    0,
                    "WindowCall",
                    3 + usize::from(filter.is_some()) + usize::from(!modifiers.is_empty()),
                )?;
                output.serialize_field("name", name)?;
                output.serialize_field(
                    "args",
                    &Expressions {
                        expressions: args,
                        values,
                    },
                )?;
                output.serialize_field("spec", &Window { spec, values })?;
                if let Some(filter) = filter {
                    output.serialize_field("filter", &self.child(filter))?;
                }
                if !modifiers.is_empty() {
                    output.serialize_field("modifiers", modifiers)?;
                }
                output.end()
            }
            ScalarExpr::Case {
                base,
                when,
                else_branch,
            } => {
                let mut output = serializer.serialize_struct_variant("ScalarExpr", 0, "Case", 3)?;
                output.serialize_field(
                    "base",
                    &base.as_deref().map(|expression| self.child(expression)),
                )?;
                output.serialize_field("when", &Arms { arms: when, values })?;
                output.serialize_field(
                    "else_branch",
                    &else_branch
                        .as_deref()
                        .map(|expression| self.child(expression)),
                )?;
                output.end()
            }
            ScalarExpr::Cast { implicit, expr, ty } => {
                let mut output = serializer.serialize_struct_variant(
                    "ScalarExpr",
                    0,
                    "Cast",
                    2 + usize::from(*implicit),
                )?;
                if *implicit {
                    output.serialize_field("implicit", implicit)?;
                }
                output.serialize_field("expr", &self.child(expr))?;
                output.serialize_field("ty", ty)?;
                output.end()
            }
            ScalarExpr::Collated {
                expr,
                ty,
                collation,
            } => {
                let mut output =
                    serializer.serialize_struct_variant("ScalarExpr", 0, "Collated", 3)?;
                output.serialize_field("expr", &self.child(expr))?;
                output.serialize_field("ty", ty)?;
                output.serialize_field("collation", collation)?;
                output.end()
            }
            ScalarExpr::InSubquery {
                expr,
                subquery,
                negated,
            } => {
                let mut output =
                    serializer.serialize_struct_variant("ScalarExpr", 0, "InSubquery", 3)?;
                output.serialize_field("expr", &self.child(expr))?;
                output.serialize_field("subquery", subquery)?;
                output.serialize_field("negated", negated)?;
                output.end()
            }
            ScalarExpr::RetainedLiteral(_) => Err(serde::ser::Error::custom(
                "grouping identity retained an unnormalized constant",
            )),
            ScalarExpr::Star
            | ScalarExpr::QualifiedStar(_)
            | ScalarExpr::Default
            | ScalarExpr::Column(_)
            | ScalarExpr::Position(_)
            | ScalarExpr::InternalColumn(_)
            | ScalarExpr::QualifiedColumn { .. }
            | ScalarExpr::Param(_)
            | ScalarExpr::ScalarSubquery(_)
            | ScalarExpr::Exists { .. } => self.expression.serialize(serializer),
        }
    }
}

struct Expressions<'a, 'c> {
    expressions: &'a [ScalarExpr],
    values: &'a ValueSerialization<'c>,
}
impl Serialize for Expressions<'_, '_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut output = serializer.serialize_seq(Some(self.expressions.len()))?;
        for expression in self.expressions {
            output.serialize_element(&Expression {
                expression,
                values: self.values,
            })?;
        }
        output.end()
    }
}

struct Orders<'a, 'c> {
    orders: &'a [crate::ScalarOrder],
    values: &'a ValueSerialization<'c>,
}
impl Serialize for Orders<'_, '_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Order<'a, 'c> {
            expr: Expression<'a, 'c>,
            descending: bool,
            nulls: Option<crate::ast::NullsOrder>,
        }
        let mut output = serializer.serialize_seq(Some(self.orders.len()))?;
        for order in self.orders {
            output.serialize_element(&Order {
                expr: Expression {
                    expression: &order.expr,
                    values: self.values,
                },
                descending: order.descending,
                nulls: order.nulls,
            })?;
        }
        output.end()
    }
}

struct Arms<'a, 'c> {
    arms: &'a [(ScalarExpr, ScalarExpr)],
    values: &'a ValueSerialization<'c>,
}
impl Serialize for Arms<'_, '_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut output = serializer.serialize_seq(Some(self.arms.len()))?;
        for (condition, value) in self.arms {
            output.serialize_element(&(
                Expression {
                    expression: condition,
                    values: self.values,
                },
                Expression {
                    expression: value,
                    values: self.values,
                },
            ))?;
        }
        output.end()
    }
}

struct CompositeSource<'a, 'c> {
    source: &'a crate::expr::composites::CompositeConstantSource,
    values: &'a ValueSerialization<'c>,
}
impl Serialize for CompositeSource<'_, '_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut output = serializer.serialize_struct("CompositeConstantSource", 2)?;
        output.serialize_field("value", &self.values.value(&self.source.value))?;
        output.serialize_field("descriptors", &self.source.descriptors)?;
        output.end()
    }
}

struct Window<'a, 'c> {
    spec: &'a crate::ScalarWindowSpec,
    values: &'a ValueSerialization<'c>,
}
impl Serialize for Window<'_, '_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut output = serializer.serialize_struct(
            "ScalarWindowSpec",
            3 + usize::from(self.spec.definition.is_some()),
        )?;
        if let Some(definition) = self.spec.definition {
            output.serialize_field("definition", &definition)?;
        }
        output.serialize_field(
            "partition_by",
            &Expressions {
                expressions: &self.spec.partition_by,
                values: self.values,
            },
        )?;
        output.serialize_field(
            "order_by",
            &Orders {
                orders: &self.spec.order_by,
                values: self.values,
            },
        )?;
        output.serialize_field(
            "frame",
            &self.spec.frame.as_ref().map(|frame| Frame {
                frame,
                values: self.values,
            }),
        )?;
        output.end()
    }
}

struct Frame<'a, 'c> {
    frame: &'a crate::ScalarWindowFrame,
    values: &'a ValueSerialization<'c>,
}
impl Serialize for Frame<'_, '_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut output = serializer.serialize_struct(
            "ScalarWindowFrame",
            4 + usize::from(!self.frame.exclusion.is_no_others()),
        )?;
        output.serialize_field("mode", &self.frame.mode)?;
        output.serialize_field(
            "start",
            &Bound {
                bound: &self.frame.start,
                values: self.values,
            },
        )?;
        output.serialize_field(
            "end",
            &Bound {
                bound: &self.frame.end,
                values: self.values,
            },
        )?;
        output.serialize_field("between", &self.frame.between)?;
        if !self.frame.exclusion.is_no_others() {
            output.serialize_field("exclusion", &self.frame.exclusion)?;
        }
        output.end()
    }
}

struct Bound<'a, 'c> {
    bound: &'a crate::ScalarFrameBound,
    values: &'a ValueSerialization<'c>,
}
impl Serialize for Bound<'_, '_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.bound {
            crate::ScalarFrameBound::Preceding(expression)
            | crate::ScalarFrameBound::Following(expression) => serializer
                .serialize_newtype_variant(
                    "ScalarFrameBound",
                    0,
                    if matches!(self.bound, crate::ScalarFrameBound::Preceding(_)) {
                        "Preceding"
                    } else {
                        "Following"
                    },
                    &Expression {
                        expression,
                        values: self.values,
                    },
                ),
            bound => bound.serialize(serializer),
        }
    }
}
