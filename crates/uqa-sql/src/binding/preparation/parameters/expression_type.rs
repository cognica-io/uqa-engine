//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    AnalysisType, AnalysisVec, ColumnType, Derivation, ExpressionType, NonNull, Produced,
    ProductionControl, SQLError, ScalarExpr, UnknownOrigin,
};

impl ExpressionType {
    pub(in crate::binding::preparation) fn default_collation(
        ty: Option<&ColumnType>,
    ) -> Derivation {
        Derivation::implicit(ty.map_or(0, crate::catalog::type_metadata::pg_type_collation_oid))
    }

    pub(in crate::binding::preparation) fn combined(
        ty: Option<ColumnType>,
        values: &[Self],
    ) -> Self {
        Self::combined_owned(ty.map(AnalysisType::ordinary), values)
    }

    pub(in crate::binding::preparation) fn combined_owned(
        ty: Option<AnalysisType>,
        values: &[Self],
    ) -> Self {
        let selected = values.iter().fold(Derivation::None, |state, value| {
            state.combine(value.derivation)
        });
        let mut result = Self::resolved_owned(ty);
        result.derivation = selected.result(result.derivation.oid());
        result
    }

    pub(in crate::binding::preparation) fn comparison(left: &Self, right: &Self) -> Derivation {
        if let (Some(left), Some(right)) = (&left.row_constructor, &right.row_constructor) {
            left.iter()
                .zip(right)
                .fold(Derivation::None, |state, (left, right)| {
                    state.combine(left.derivation.combine(right.derivation).error_only())
                })
        } else {
            left.derivation.combine(right.derivation).error_only()
        }
    }

    pub(in crate::binding::preparation) fn independent(
        ty: Option<ColumnType>,
        values: &[Self],
    ) -> Self {
        Self::independent_owned(ty.map(AnalysisType::ordinary), values)
    }

    pub(in crate::binding::preparation) fn independent_owned(
        ty: Option<AnalysisType>,
        values: &[Self],
    ) -> Self {
        let mut result = Self::resolved_owned(ty);
        result.derivation = values.iter().fold(result.derivation, |state, value| {
            state.combine(value.derivation.error_only())
        });
        result
    }

    pub(in crate::binding::preparation) fn resolved(ty: Option<ColumnType>) -> Self {
        Self {
            derivation: Self::default_collation(ty.as_ref()),
            initial_collation: 0,
            ty: ty.map(AnalysisType::ordinary),
            record_fields: None,
            record_descriptor: None,
            record_variable: false,
            row_constructor: None,
            occurrence: None,
            unknown: UnknownOrigin::Deferred,
            literal: None,
            collation: None,
        }
    }

    pub(in crate::binding::preparation) fn unknown() -> Self {
        Self {
            unknown: UnknownOrigin::Literal,
            ..Self::resolved(None)
        }
    }

    /// A bare string literal, `unknown` until its context selects a type that reads it.
    #[cfg(test)]
    pub(in crate::binding::preparation) fn unknown_literal(
        expression: &ScalarExpr,
        text: String,
    ) -> Self {
        Self::unknown_literal_produced(
            expression,
            ProductionControl::uncontrolled()
                .finish(text, None)
                .expect("ordinary unknown input"),
        )
    }

    pub(in crate::binding::preparation) fn unknown_literal_produced(
        expression: &ScalarExpr,
        text: Produced<String>,
    ) -> Self {
        Self {
            literal: Some((NonNull::from(expression), text)),
            ..Self::unknown()
        }
    }

    pub(in crate::binding::preparation) fn resolved_produced(
        ty: Option<Produced<ColumnType>>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let ty = ty
            .map(|ty| AnalysisType::from_produced(ty, control))
            .transpose()?;
        Ok(Self::resolved_owned(ty))
    }

    pub(in crate::binding::preparation) fn resolved_owned(ty: Option<AnalysisType>) -> Self {
        let derivation = Self::default_collation(ty.as_deref());
        Self {
            ty,
            derivation,
            ..Self::resolved(None)
        }
    }

    pub(in crate::binding::preparation) fn replace_type(
        &mut self,
        ty: Option<Produced<ColumnType>>,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        self.ty = ty
            .map(|ty| AnalysisType::from_produced(ty, control))
            .transpose()?;
        Ok(())
    }

    pub(in crate::binding::preparation) fn copy_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        let mut output = Self::resolved_owned(self.ty.clone());
        output.derivation = self.derivation;
        output.initial_collation = self.initial_collation;
        output.record_fields.clone_from(&self.record_fields);
        output.record_descriptor.clone_from(&self.record_descriptor);
        output.record_variable = self.record_variable;
        output.occurrence = self.occurrence;
        output.unknown = self.unknown;
        output.collation = self.collation;
        output.literal = self
            .literal
            .as_ref()
            .map(|(identity, text)| Ok::<_, SQLError>((*identity, control.copy_text(text)?)))
            .transpose()?;
        output.row_constructor = self
            .row_constructor
            .as_ref()
            .map(|fields| {
                let mut output = AnalysisVec::new(control);
                output.reserve(fields.len(), control)?;
                for field in fields {
                    output.push(field.copy_with_control(control)?, control)?;
                }
                Ok::<_, SQLError>(output)
            })
            .transpose()?;
        Ok(output)
    }

    pub(in crate::binding::preparation) fn is_deferred(&self) -> bool {
        self.ty.is_none() && self.unknown == UnknownOrigin::Deferred
    }

    pub(in crate::binding::preparation) fn is_sql_unknown(&self) -> bool {
        self.ty.is_none() && self.unknown != UnknownOrigin::Deferred
    }

    pub(in crate::binding::preparation) fn is_unknown_parameter(&self) -> bool {
        self.ty.is_none() && self.unknown == UnknownOrigin::Parameter
    }

    pub(in crate::binding::preparation) fn mark_unknown_field(&mut self) {
        self.unknown = UnknownOrigin::Field;
    }

    pub(in crate::binding::preparation) fn leaf_owned(ty: Option<AnalysisType>) -> Self {
        let mut value = Self::resolved_owned(ty);
        value.initial_collation =
            u32::try_from(value.derivation.oid()).expect("type collation OID");
        value
    }

    pub(in crate::binding::preparation) fn record_metadata(
        &self,
    ) -> Option<crate::schema::RecordMetadata> {
        (self.record_fields.is_some() || self.record_descriptor.is_some()).then(|| {
            crate::schema::RecordMetadata {
                types: self.record_fields.clone(),
                descriptor: self.record_descriptor.clone(),
            }
        })
    }
}
