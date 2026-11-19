//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Input, InputError, JsonToken, Parts, Produced, Values};
use crate::ast::{
    stored_input::{invalid, number, unit},
    PartitionBound, PartitionRangeDatum, PartitionSpec,
};

impl<'a> Input<'a, '_> {
    pub(super) fn partition_spec(
        &mut self,
        token: JsonToken<'a>,
        values: &Values<'_>,
    ) -> Result<Produced<PartitionSpec>, InputError> {
        let (mut strategy, mut keys) = (None, None);
        self.fields(token, &["strategy", "keys"], 3, |input, index, token| {
            if index == 0 {
                strategy = Some(unit(&input.unit_tag(token)?)?);
            } else {
                keys = Some(
                    input.array(token, |input, token| input.schema_expression(token, values))?,
                );
            }
            Ok(())
        })?;
        let mut parts = Parts::new(self.control);
        let value = PartitionSpec {
            strategy: strategy.expect("required partition strategy"),
            keys: parts.take(keys.expect("required partition keys")),
        };
        parts.finish(value)
    }

    pub(super) fn partition_bound(
        &mut self,
        token: JsonToken<'a>,
        values: &Values<'_>,
    ) -> Result<Produced<PartitionBound>, InputError> {
        let (tag, object) = self.tag(token)?;
        let mut parts = Parts::new(self.control);
        let value = if tag.as_str() == "Default" {
            if object && self.next()? != JsonToken::Null {
                return Err(invalid());
            }
            PartitionBound::Default
        } else {
            if !object {
                return Err(invalid());
            }
            let token = self.next()?;
            match tag.as_str() {
                "List" => PartitionBound::List(parts.take(
                    self.array(token, |input, token| input.schema_expression(token, values))?,
                )),
                "Range" => {
                    let (mut lower, mut upper) = (None, None);
                    self.fields(token, &["lower", "upper"], 3, |input, index, token| {
                        let value = input
                            .array(token, |input, token| input.partition_datum(token, values))?;
                        if index == 0 {
                            lower = Some(value);
                        } else {
                            upper = Some(value);
                        }
                        Ok(())
                    })?;
                    PartitionBound::Range {
                        lower: parts.take(lower.expect("required lower partition bound")),
                        upper: parts.take(upper.expect("required upper partition bound")),
                    }
                }
                "Hash" => {
                    let (mut modulus, mut remainder) = (0, 0);
                    self.fields(token, &["modulus", "remainder"], 3, |_, index, token| {
                        if index == 0 {
                            modulus = number(token)?;
                        } else {
                            remainder = number(token)?;
                        }
                        Ok(())
                    })?;
                    PartitionBound::Hash { modulus, remainder }
                }
                _ => return Err(invalid()),
            }
        };
        self.close_tag(object)?;
        parts.finish(value)
    }

    fn partition_datum(
        &mut self,
        token: JsonToken<'a>,
        values: &Values<'_>,
    ) -> Result<Produced<PartitionRangeDatum>, InputError> {
        let (tag, object) = self.tag(token)?;
        let mut parts = Parts::new(self.control);
        let value = match tag.as_str() {
            "MinValue" | "MaxValue" => {
                if object && self.next()? != JsonToken::Null {
                    return Err(invalid());
                }
                if tag.as_str() == "MinValue" {
                    PartitionRangeDatum::MinValue
                } else {
                    PartitionRangeDatum::MaxValue
                }
            }
            "Value" if object => {
                let token = self.next()?;
                PartitionRangeDatum::Value(parts.take(self.schema_expression(token, values)?))
            }
            _ => return Err(invalid()),
        };
        self.close_tag(object)?;
        parts.finish(value)
    }
}
