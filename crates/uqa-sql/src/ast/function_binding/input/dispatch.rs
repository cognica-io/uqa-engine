//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::ast::stored_input::InputError;
use crate::ast::{
    stored_input::{boolean, invalid, number, optional, unit, Input},
    ArraySliceBounds, FunctionDispatch,
};
use uqa_core::json::JsonToken;

impl<'a> Input<'a, '_> {
    pub(super) fn dispatch(
        &mut self,
        token: JsonToken<'a>,
    ) -> Result<FunctionDispatch, InputError> {
        let (tag, object) = self.tag(token)?;
        let value = if matches!(
            tag.as_str(),
            "NumericOperator"
                | "BooleanTest"
                | "ArraySlicesWithBounds"
                | "Pattern"
                | "Range"
                | "Enum"
                | "JsonPath"
                | "JsonExtract"
                | "CollationFor"
                | "PatternEscape"
                | "ValueNullTest"
                | "Collate"
                | "Concat"
                | "TypeOf"
        ) {
            if !object {
                return Err(invalid());
            }
            let token = self.next()?;
            self.dispatch_payload(&tag, token)?
        } else {
            let value = unit::<FunctionDispatch>(&tag)?;
            if object && self.next()? != JsonToken::Null {
                return Err(invalid());
            }
            value
        };
        self.close_tag(object)?;
        Ok(value)
    }

    fn dispatch_payload(
        &mut self,
        tag: &str,
        token: JsonToken<'a>,
    ) -> Result<FunctionDispatch, InputError> {
        use FunctionDispatch as D;
        match tag {
            "NumericOperator" => Ok(D::NumericOperator(unit(&self.unit_tag(token)?)?)),
            "BooleanTest" => Ok(D::BooleanTest(unit(&self.unit_tag(token)?)?)),
            "ArraySlicesWithBounds" => Ok(D::ArraySlicesWithBounds(self.slice_bounds(token)?)),
            "Pattern" => self.pattern_dispatch(token),
            "Range" => self.range_dispatch(token),
            "Enum" => self.enum_dispatch(token),
            "JsonPath" | "JsonExtract" => self.json_dispatch(tag, token),
            "CollationFor" => self.collation_dispatch(token),
            "PatternEscape" | "ValueNullTest" => {
                let mut flag = false;
                let field = if tag == "PatternEscape" {
                    "similar"
                } else {
                    "negated"
                };
                self.fields(token, &[field], 1, |_, _, value| {
                    flag = boolean(value)?;
                    Ok(())
                })?;
                Ok(if tag == "PatternEscape" {
                    D::PatternEscape { similar: flag }
                } else {
                    D::ValueNullTest { negated: flag }
                })
            }
            "Collate" | "Concat" | "TypeOf" => {
                let mut oid = None;
                let field = if tag == "TypeOf" { "type_oid" } else { "oid" };
                self.fields(
                    token,
                    &[field],
                    u64::from(tag == "TypeOf"),
                    |_, _, value| {
                        oid = if tag == "TypeOf" {
                            Some(number(value)?)
                        } else {
                            optional(value)?
                        };
                        Ok(())
                    },
                )?;
                Ok(match tag {
                    "Collate" => D::Collate { oid },
                    "Concat" => D::Concat { oid },
                    _ => D::TypeOf {
                        type_oid: oid.ok_or_else(invalid)?,
                    },
                })
            }
            _ => Err(invalid()),
        }
    }

    pub(super) fn slice_bounds(
        &mut self,
        token: JsonToken<'a>,
    ) -> Result<ArraySliceBounds, InputError> {
        let mut masks = [0; 2];
        self.fields(
            token,
            &["omitted_lower", "omitted_upper"],
            3,
            |_, index, token| {
                masks[index] = number(token)?;
                Ok(())
            },
        )?;
        Ok(ArraySliceBounds::from_stored_masks(masks))
    }

    fn pattern_dispatch(&mut self, token: JsonToken<'a>) -> Result<FunctionDispatch, InputError> {
        let (mut operator, mut oid) = (None, None);
        self.fields(token, &["operator", "oid"], 1, |input, index, token| {
            if index == 0 {
                operator = Some(unit(&input.unit_tag(token)?)?);
            } else {
                oid = optional(token)?
                    .map(|value| std::num::NonZeroU32::new(value).ok_or_else(invalid))
                    .transpose()?;
            }
            Ok(())
        })?;
        Ok(FunctionDispatch::Pattern {
            operator: operator.ok_or_else(invalid)?,
            oid,
        })
    }

    fn range_dispatch(&mut self, token: JsonToken<'a>) -> Result<FunctionDispatch, InputError> {
        let (mut operation, mut subtype, mut multirange) = (None, None, false);
        self.fields(
            token,
            &["operation", "subtype", "multirange"],
            7,
            |input, index, token| {
                match index {
                    0 => operation = Some(unit(&input.unit_tag(token)?)?),
                    1 => subtype = Some(unit(&input.unit_tag(token)?)?),
                    _ => multirange = boolean(token)?,
                }
                Ok(())
            },
        )?;
        Ok(FunctionDispatch::Range {
            operation: operation.ok_or_else(invalid)?,
            subtype: subtype.ok_or_else(invalid)?,
            multirange,
        })
    }

    fn enum_dispatch(&mut self, token: JsonToken<'a>) -> Result<FunctionDispatch, InputError> {
        let (mut operation, mut type_oid) = (None, 0);
        self.fields(
            token,
            &["operation", "type_oid"],
            3,
            |input, index, token| {
                if index == 0 {
                    operation = Some(unit(&input.unit_tag(token)?)?);
                } else {
                    type_oid = number(token)?;
                }
                Ok(())
            },
        )?;
        Ok(FunctionDispatch::Enum {
            operation: operation.ok_or_else(invalid)?,
            type_oid,
        })
    }

    fn json_dispatch(
        &mut self,
        tag: &str,
        token: JsonToken<'a>,
    ) -> Result<FunctionDispatch, InputError> {
        let mut flags = [false; 2];
        let second = if tag == "JsonPath" { "jsonb" } else { "path" };
        self.fields(token, &["as_text", second], 3, |_, index, token| {
            flags[index] = boolean(token)?;
            Ok(())
        })?;
        Ok(if tag == "JsonPath" {
            FunctionDispatch::JsonPath {
                as_text: flags[0],
                jsonb: flags[1],
            }
        } else {
            FunctionDispatch::JsonExtract {
                as_text: flags[0],
                path: flags[1],
            }
        })
    }

    fn collation_dispatch(&mut self, token: JsonToken<'a>) -> Result<FunctionDispatch, InputError> {
        let (mut collation_oid, mut noncollatable_type_oid) = (0, None);
        self.fields(
            token,
            &["collation_oid", "noncollatable_type_oid"],
            1,
            |_, index, token| {
                if index == 0 {
                    collation_oid = number(token)?;
                } else {
                    noncollatable_type_oid = optional(token)?;
                }
                Ok(())
            },
        )?;
        Ok(FunctionDispatch::CollationFor {
            collation_oid,
            noncollatable_type_oid,
        })
    }
}
