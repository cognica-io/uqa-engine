//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored executable scalars retain selected metadata and query-arena addresses.

use super::{invalid, DefinitionImageReader, Frame, SQLError};
use crate::{
    ast::{FrameExclusion, FrameMode, FunctionCallSyntax, NullsOrder},
    catalog::collations::derivation::Derivation,
    ir::{ResultCollation, ScalarExpr, ScalarFrameBound, ScalarOrder, ScalarWindowSpec},
};
use uqa_core::{memory::MemoryBudget, CancellationToken};

pub(in super::super) const MAGIC: [u8; 8] = *b"UQASCL01";

pub(super) enum Part<'a> {
    Node(&'a ScalarExpr),
    Nodes(&'a [ScalarExpr]),
    Orders(&'a [ScalarOrder]),
    Cases(&'a [(ScalarExpr, ScalarExpr)]),
}

impl ScalarExpr {
    /// Reconstruct native executable metadata under the destination allowance. The enclosing query owner validates arena references before publication.
    pub fn read_image(
        reader: &mut dyn std::io::Read,
        memory: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<super::super::input::CapturedScalar, SQLError> {
        super::super::input::scalar_image(reader, memory, cancellation)
    }

    /// Borrow a captured executable scalar without copying it, resolving names or reading external constants. Query references retain their original enclosing-arena indices.
    pub fn image_reader<'a>(
        &'a self,
        memory: &'a MemoryBudget,
        cancellation: &'a CancellationToken,
    ) -> Result<DefinitionImageReader<'a>, SQLError> {
        DefinitionImageReader::start(
            Frame::Scalar(Part::Node(self)),
            MAGIC,
            1,
            memory,
            cancellation,
        )
    }
}

impl<'a> DefinitionImageReader<'a> {
    pub(super) fn scalar_part(&mut self, part: Part<'a>) -> Result<(), SQLError> {
        match part {
            Part::Node(value) => self.scalar_node(value)?,
            Part::Nodes(values) => {
                if let Some((first, rest)) = values.split_first() {
                    self.scalar_push(Part::Nodes(rest))?;
                    self.scalar_push(Part::Node(first))?;
                }
            }
            Part::Orders(values) => {
                if let Some((first, rest)) = values.split_first() {
                    self.fields.flag(first.descending);
                    self.fields.scalar([match first.nulls {
                        None => 0,
                        Some(NullsOrder::First) => 1,
                        Some(NullsOrder::Last) => 2,
                    }]);
                    self.scalar_push(Part::Orders(rest))?;
                    self.scalar_push(Part::Node(&first.expr))?;
                }
            }
            Part::Cases(values) => {
                if let Some(((condition, result), rest)) = values.split_first() {
                    self.scalar_push(Part::Cases(rest))?;
                    self.scalar_push(Part::Node(result))?;
                    self.scalar_push(Part::Node(condition))?;
                }
            }
        }
        Ok(())
    }

    fn scalar_push(&mut self, part: Part<'a>) -> Result<(), SQLError> {
        self.push(Frame::Scalar(part))
    }

    fn optional_scalar(&mut self, value: Option<&'a ScalarExpr>) -> Result<(), SQLError> {
        if let Some(value) = value {
            self.scalar_push(Part::Node(value))?;
        }
        Ok(())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "exhaustive native scalar constructors"
    )]
    fn scalar_node(&mut self, value: &'a ScalarExpr) -> Result<(), SQLError> {
        self.fields.scalar([tag(value)]);
        match value {
            ScalarExpr::Star | ScalarExpr::Default => {}
            ScalarExpr::QualifiedStar(name) | ScalarExpr::Column(name) => self.fields.text(name),
            ScalarExpr::QualifiedColumn { qualifier, column } => {
                self.fields.text(qualifier);
                self.fields.text(column);
            }
            ScalarExpr::InternalColumn(column) => {
                self.fields.scalar(column.relation().raw().to_le_bytes());
                self.fields
                    .scalar((column.attribute() as u32).to_le_bytes());
            }
            ScalarExpr::Literal(value) => self.push(Frame::Value(value))?,
            ScalarExpr::TypedLiteral {
                value,
                ty,
                collation_oid,
                composite_source,
                bound_type,
                parameter_index,
            } => {
                self.fields.text(ty);
                self.fields.optional_u32(*collation_oid);
                self.fields.flag(composite_source.is_some());
                self.fields.flag(bound_type.is_some());
                self.fields.flag(parameter_index.is_some());
                if let Some(index) = parameter_index {
                    self.fields.count(*index);
                }
                if let Some(source) = composite_source {
                    self.push(Frame::CompositeSource(source))?;
                }
                if let Some(ty) = bound_type {
                    self.push(Frame::Type(ty))?;
                }
                self.push(Frame::Value(value))?;
            }
            ScalarExpr::Param(index)
            | ScalarExpr::Position(index)
            | ScalarExpr::ScalarSubquery(index) => self.fields.count(*index),
            ScalarExpr::Func {
                name,
                binding,
                args,
                distinct,
                order_by,
                order_syntax,
                filter,
            } => {
                self.fields.text(name);
                self.fields.flag(binding.is_some());
                self.fields.flag(*distinct);
                self.fields.scalar([match order_syntax {
                    FunctionCallSyntax::Legacy => 0,
                    FunctionCallSyntax::Ordinary => 1,
                    FunctionCallSyntax::WithinGroup => 2,
                    FunctionCallSyntax::Extract => 3,
                }]);
                self.fields.count(args.len());
                self.fields.count(order_by.len());
                self.fields.flag(filter.is_some());
                self.optional_scalar(filter.as_deref())?;
                self.scalar_push(Part::Orders(order_by))?;
                self.scalar_push(Part::Nodes(args))?;
                if let Some(binding) = binding {
                    self.push(Frame::Binding(binding))?;
                }
            }
            ScalarExpr::Array(values)
            | ScalarExpr::Row(values)
            | ScalarExpr::And(values)
            | ScalarExpr::Or(values) => {
                self.fields.count(values.len());
                self.scalar_push(Part::Nodes(values))?;
            }
            ScalarExpr::CompositeRow {
                items,
                binding,
                bound_type,
            } => {
                self.fields.count(items.len());
                self.fields.flag(bound_type.is_some());
                self.scalar_push(Part::Nodes(items))?;
                if let Some(ty) = bound_type {
                    self.push(Frame::Type(ty))?;
                }
                self.row_binding(binding)?;
            }
            ScalarExpr::Binary { op, lhs, rhs } => {
                self.fields.scalar([super::nodes::binary_tag(*op)]);
                self.scalar_push(Part::Node(rhs))?;
                self.scalar_push(Part::Node(lhs))?;
            }
            ScalarExpr::UnaryMinus(inner) | ScalarExpr::Not(inner) => {
                self.scalar_push(Part::Node(inner))?;
            }
            ScalarExpr::IsNull { expr, negated } => {
                self.fields.flag(*negated);
                self.scalar_push(Part::Node(expr))?;
            }
            ScalarExpr::Between { expr, low, high } => {
                self.scalar_push(Part::Node(high))?;
                self.scalar_push(Part::Node(low))?;
                self.scalar_push(Part::Node(expr))?;
            }
            ScalarExpr::InList {
                expr,
                list,
                negated,
            } => {
                self.fields.flag(*negated);
                self.fields.count(list.len());
                self.scalar_push(Part::Nodes(list))?;
                self.scalar_push(Part::Node(expr))?;
            }
            ScalarExpr::Case {
                base,
                when,
                else_branch,
            } => {
                self.fields.flag(base.is_some());
                self.fields.count(when.len());
                self.fields.flag(else_branch.is_some());
                self.optional_scalar(else_branch.as_deref())?;
                self.scalar_push(Part::Cases(when))?;
                self.optional_scalar(base.as_deref())?;
            }
            ScalarExpr::Cast { implicit, expr, ty } => {
                self.fields.flag(*implicit);
                self.fields.text(ty);
                self.scalar_push(Part::Node(expr))?;
            }
            ScalarExpr::RetainedLiteral(value) => self.retained_scalar_literal(value)?,
            ScalarExpr::Collated {
                expr,
                ty,
                collation,
            } => {
                self.scalar_collation(*collation)?;
                self.scalar_push(Part::Node(expr))?;
                self.push(Frame::Type(ty))?;
            }
            ScalarExpr::Exists { subquery, negated } => {
                self.fields.count(*subquery);
                self.fields.flag(*negated);
            }
            ScalarExpr::InSubquery {
                expr,
                subquery,
                negated,
            } => {
                self.fields.count(*subquery);
                self.fields.flag(*negated);
                self.scalar_push(Part::Node(expr))?;
            }
            ScalarExpr::WindowCall {
                name,
                args,
                spec,
                filter,
                modifiers,
            } => {
                self.fields.text(name);
                self.fields.count(args.len());
                self.fields.flag(filter.is_some());
                self.fields.flag(modifiers.distinct);
                self.fields.flag(modifiers.ordered);
                self.fields.flag(modifiers.within_group);
                self.optional_scalar(filter.as_deref())?;
                self.scalar_window(spec)?;
                self.scalar_push(Part::Nodes(args))?;
            }
        }
        Ok(())
    }

    fn scalar_collation(&mut self, value: ResultCollation) -> Result<(), SQLError> {
        let (tag, left, right) = match value.derivation() {
            Derivation::None => (0, 0, 0),
            Derivation::Implicit(oid) => (1, oid, 0),
            Derivation::Explicit(oid) => (2, oid, 0),
            Derivation::Conflict(left, right) => (3, left, right),
            Derivation::InvalidImplicit(..) | Derivation::InvalidExplicit(..) => {
                return Err(invalid("unresolved scalar collation"))
            }
        };
        let left = u32::try_from(left).map_err(|_| invalid("scalar collation identifier"))?;
        let right = u32::try_from(right).map_err(|_| invalid("scalar collation identifier"))?;
        self.fields.scalar([tag]);
        self.fields.scalar(left.to_le_bytes());
        self.fields.scalar(right.to_le_bytes());
        self.fields.scalar(value.initial_oid().to_le_bytes());
        Ok(())
    }

    fn scalar_window(&mut self, value: &'a ScalarWindowSpec) -> Result<(), SQLError> {
        self.fields.flag(value.definition.is_some());
        if let Some(index) = value.definition {
            self.fields.count(index);
        }
        self.fields.count(value.partition_by.len());
        self.fields.count(value.order_by.len());
        self.fields.flag(value.frame.is_some());
        if let Some(frame) = &value.frame {
            self.fields.scalar([match frame.mode {
                FrameMode::Rows => 0,
                FrameMode::Range => 1,
                FrameMode::Groups => 2,
            }]);
            self.fields.scalar([bound_tag(&frame.start)]);
            self.fields.scalar([bound_tag(&frame.end)]);
            self.fields.flag(frame.between);
            self.fields.scalar([match frame.exclusion {
                FrameExclusion::NoOthers => 0,
                FrameExclusion::CurrentRow => 1,
                FrameExclusion::Group => 2,
                FrameExclusion::Ties => 3,
            }]);
            for bound in [&frame.end, &frame.start] {
                if let ScalarFrameBound::Preceding(value) | ScalarFrameBound::Following(value) =
                    bound
                {
                    self.scalar_push(Part::Node(value))?;
                }
            }
        }
        self.scalar_push(Part::Orders(&value.order_by))?;
        self.scalar_push(Part::Nodes(&value.partition_by))?;
        Ok(())
    }
}

fn bound_tag(value: &ScalarFrameBound) -> u8 {
    match value {
        ScalarFrameBound::UnboundedPreceding => 0,
        ScalarFrameBound::UnboundedFollowing => 1,
        ScalarFrameBound::CurrentRow => 2,
        ScalarFrameBound::Preceding(_) => 3,
        ScalarFrameBound::Following(_) => 4,
    }
}

fn tag(value: &ScalarExpr) -> u8 {
    match value {
        ScalarExpr::Star => 0,
        ScalarExpr::QualifiedStar(_) => 1,
        ScalarExpr::Default => 2,
        ScalarExpr::Column(_) => 3,
        ScalarExpr::QualifiedColumn { .. } => 4,
        ScalarExpr::InternalColumn(_) => 5,
        ScalarExpr::Literal(_) => 6,
        ScalarExpr::TypedLiteral { .. } => 7,
        ScalarExpr::Param(_) => 8,
        ScalarExpr::Func { .. } => 9,
        ScalarExpr::Array(_) => 10,
        ScalarExpr::Row(_) => 11,
        ScalarExpr::CompositeRow { .. } => 12,
        ScalarExpr::Binary { .. } => 13,
        ScalarExpr::UnaryMinus(_) => 14,
        ScalarExpr::Not(_) => 15,
        ScalarExpr::And(_) => 16,
        ScalarExpr::Or(_) => 17,
        ScalarExpr::IsNull { .. } => 18,
        ScalarExpr::Between { .. } => 19,
        ScalarExpr::InList { .. } => 20,
        ScalarExpr::Case { .. } => 21,
        ScalarExpr::Cast { .. } => 22,
        ScalarExpr::RetainedLiteral(_) => 23,
        ScalarExpr::Position(_) => 24,
        ScalarExpr::Collated { .. } => 25,
        ScalarExpr::ScalarSubquery(_) => 26,
        ScalarExpr::Exists { .. } => 27,
        ScalarExpr::InSubquery { .. } => 28,
        ScalarExpr::WindowCall { .. } => 29,
    }
}
