//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, Child, Expr, Input, InputError, Parts, Slot, Values};
use crate::ast::{stored_input::number, InternalColumnRef};
use uqa_core::{
    json::JsonToken,
    memory::{BudgetedVec, Produced},
};

impl<'a> Input<'a, '_> {
    pub(super) fn schema_node(
        &mut self,
        token: JsonToken<'a>,
        children: &mut BudgetedVec<Child<'a>>,
        values: &Values<'_>,
    ) -> Result<Produced<Expr>, InputError> {
        let (tag, object) = self.tag(token)?;
        let value = if matches!(tag.as_str(), "Star" | "Default") {
            if object && self.next()? != JsonToken::Null {
                return Err(invalid());
            }
            let value = if tag.as_str() == "Star" {
                Expr::Star
            } else {
                Expr::Default
            };
            self.control
                .finish(value, self.control.empty_reservation())?
        } else {
            if !object {
                return Err(invalid());
            }
            let token = self.next()?;
            self.schema_payload(&tag, token, children, values)?
        };
        self.close_tag(object)?;
        Ok(value)
    }

    fn schema_payload(
        &mut self,
        tag: &str,
        token: JsonToken<'a>,
        children: &mut BudgetedVec<Child<'a>>,
        values: &Values<'_>,
    ) -> Result<Produced<Expr>, InputError> {
        let mut parts = Parts::new(self.control);
        let value = match tag {
            "Column" => Expr::Column(parts.take(self.text(token)?).into()),
            "QualifiedStar" => Expr::QualifiedStar(parts.take(self.text(token)?).into()),
            "QualifiedColumn" => return self.qualified_column(token),
            "InternalColumn" => Expr::InternalColumn(self.internal_column(token)?),
            "Param" => Expr::Param(number(token)?),
            "Literal" => Expr::Literal(parts.take(values.value(self, token)?)),
            "TypedLiteral" => return self.typed_literal(token, values),
            "UnaryMinus" => {
                Expr::UnaryMinus(parts.take(self.child_box(token, children, Slot::First)?))
            }
            "Not" => Expr::Not(parts.take(self.child_box(token, children, Slot::First)?)),
            "Array" | "Row" | "And" | "Or" => {
                let items = parts.take(self.items(token, children)?);
                match tag {
                    "Array" => Expr::Array(items),
                    "Row" => Expr::Row(items),
                    "And" => Expr::And(items),
                    _ => Expr::Or(items),
                }
            }
            "CompositeRow" => return self.composite_row(token, children),
            "Binary" => return self.binary_expression(token, children),
            "IsNull" | "InList" | "Between" | "Cast" => {
                return self.structured_expression(tag, token, children);
            }
            "Case" => return self.case_expression(token, children),
            "Func" => return self.function_expression(token, children),
            // Query and window nodes have no stored-schema expression contract; their owning query decoder remains separate.
            _ => return Err(invalid()),
        };
        parts.finish(value)
    }

    fn qualified_column(&mut self, token: JsonToken<'a>) -> Result<Produced<Expr>, InputError> {
        let (mut qualifier, mut column) = (None, None);
        self.fields(token, &["qualifier", "column"], 3, |input, index, token| {
            if index == 0 {
                qualifier = Some(input.text(token)?);
            } else {
                column = Some(input.text(token)?);
            }
            Ok(())
        })?;
        let mut parts = Parts::new(self.control);
        let value = Expr::QualifiedColumn {
            qualifier: parts.take(qualifier.expect("required qualifier")),
            column: parts.take(column.expect("required column")).into(),
        };
        parts.finish(value)
    }

    fn internal_column(&mut self, token: JsonToken<'a>) -> Result<InternalColumnRef, InputError> {
        let (mut relation, mut attribute) = (0, 0);
        self.fields(token, &["relation", "attribute"], 3, |_, index, token| {
            if index == 0 {
                relation = number(token)?;
            } else {
                attribute = number(token)?;
            }
            Ok(())
        })?;
        Ok(InternalColumnRef::from_raw(relation, attribute))
    }
}
