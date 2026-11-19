//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Text inputs are produced under the same allowance as their retained executable literals.

use super::{
    AnalysisIdentity, ColumnType, ConvertedInput, OwnedScalarExpr, ParameterTypes, Produced,
    SQLError, ScalarExpr, Value,
};

impl<I: super::LiteralInput> ParameterTypes<'_, I> {
    pub(super) fn read_literal(
        &mut self,
        origin: AnalysisIdentity<ScalarExpr>,
        text: &str,
        target: &ColumnType,
    ) -> Result<(), SQLError> {
        let owner = self.control.clone();
        let control = owner.control();
        let input_type = crate::type_resolution::literal_input_type_with_control(target, &control)?;
        if let Some(previous) = self
            .input_constants
            .as_ref()
            .and_then(|constants| constants.0.get(&origin))
        {
            if let ScalarExpr::TypedLiteral { ty, bound_type, .. } = &*previous.expression {
                let same = match bound_type {
                    Some(previous) => previous == &*input_type,
                    None if self.stored_literal_syntax => {
                        ty == &*input_type.catalog_name_with_control(&control)?
                    }
                    None => false,
                };
                if same {
                    return Ok(());
                }
            }
        }
        if let Some(input) = self.input_value(text, &input_type)? {
            if let Some(constants) = &mut self.input_constants {
                let name = input_type.catalog_name_with_control(&control)?;
                let expression = OwnedScalarExpr::typed_literal(
                    input.value,
                    name,
                    (!self.stored_literal_syntax).then_some(input_type),
                    None,
                    None,
                    &control,
                )?;
                constants.0.insert(
                    origin,
                    ConvertedInput {
                        expression,
                        session_dependent: input.session_dependent,
                    },
                )?;
            }
        }
        Ok(())
    }

    fn input_value(
        &self,
        text: &str,
        input_type: &ColumnType,
    ) -> Result<Option<ReadInput>, SQLError> {
        let control = self.control();
        control.check()?;
        if crate::expr::requires_catalog_constant_input(input_type) {
            if self.input_constants.is_none() {
                return Ok(None);
            }
            let catalog = self.inputs.catalog_inputs().ok_or_else(|| {
                SQLError::Internal(format!(
                    "catalog input functions are unavailable for {}",
                    input_type.sql_name()
                ))
            })?;
            return catalog
                .read_unknown_input_with_control(text, input_type, &control)
                .map(|value| Some(ReadInput::session_dependent(value)));
        }
        let (owned_text, memory) = control.copy_text(text)?.into_parts();
        let input = control.finish(Value::Str(owned_text), memory)?;
        if self.input_constants.is_some() && crate::expr::enums::is_enum_bearing(input_type) {
            if let Some(value) = crate::expr::enums::fold_unknown_literal_with_control(
                self.inputs.enum_labels(),
                &input,
                input_type,
                &control,
            )? {
                return Ok(Some(ReadInput::session_dependent(value)));
            }
        }
        if crate::type_resolution::catalog_input_type(input_type) {
            return self.inputs.aliases().map_or(Ok(None), |catalog| {
                crate::schema::dependencies::oid_alias::read_unknown_constant_with_control(
                    catalog, input_type, text, &control,
                )
                .map(|value| value.map(ReadInput::session_dependent))
            });
        }
        let name = input_type.catalog_name_with_control(&control)?;
        let value = crate::expr::cast_value_from_with_control(&input, &name, None, &control)?;
        let session_dependent = super::input_reads_session(text, input_type, &value, &control)?;
        Ok(Some(ReadInput {
            value,
            session_dependent,
        }))
    }
}

struct ReadInput {
    value: Produced<Value>,
    session_dependent: bool,
}

impl ReadInput {
    fn session_dependent(value: Produced<Value>) -> Self {
        Self {
            value,
            session_dependent: true,
        }
    }
}
