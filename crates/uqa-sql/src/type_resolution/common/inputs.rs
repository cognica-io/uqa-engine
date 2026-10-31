//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Common input selection preserves domains and modifiers only across identical inputs.

use super::{
    base_type, select_pair_with_control, ColumnType, CommonTypeContext, CommonTypeFailure,
};
use crate::SQLError;
use uqa_core::memory::{Produced, ProductionControl};

/// Retain one selected type while consuming original input types, including SQL `unknown`.
pub(in crate::type_resolution) struct CommonInputType {
    selected: Option<Produced<ColumnType>>,
    same_type: bool,
}

impl CommonInputType {
    pub(in crate::type_resolution) const fn new() -> Self {
        Self {
            selected: None,
            same_type: true,
        }
    }

    fn from_inputs<'a>(
        mut types: impl Iterator<Item = Option<&'a ColumnType>>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        let mut same_type = false;
        if let Some(Some(first)) = types.next() {
            same_type = true;
            for ty in types {
                control.check()?;
                if ty != Some(first) {
                    same_type = false;
                    break;
                }
            }
        }
        Ok(Self {
            selected: None,
            same_type,
        })
    }

    pub(in crate::type_resolution) fn include(
        &mut self,
        context: CommonTypeContext,
        ty: Option<&ColumnType>,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        self.select(ty, control)
            .map_err(|failure| failure.in_context(context))
    }

    pub(in crate::type_resolution) fn include_owned(
        &mut self,
        context: CommonTypeContext,
        ty: Option<Produced<ColumnType>>,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        if self.selected.is_none() && self.same_type && ty.is_some() {
            control.check()?;
            self.selected = ty;
            return Ok(());
        }
        self.include(context, ty.as_deref(), control)
    }

    fn select(
        &mut self,
        ty: Option<&ColumnType>,
        control: &ProductionControl<'_>,
    ) -> Result<(), CommonTypeFailure> {
        control.check().map_err(CommonTypeFailure::failed)?;
        self.selected = match (self.selected.take(), ty) {
            (None, None) => {
                self.same_type = false;
                None
            }
            (None, Some(ty)) => Some(
                if self.same_type {
                    ty.clone_with_control(control)
                } else {
                    base_type(ty).without_type_modifiers_with_control(control)
                }
                .map_err(CommonTypeFailure::failed)?,
            ),
            (Some(previous), None) => {
                self.same_type = false;
                Some(
                    if *previous == *base_type(&previous) && !previous.has_type_modifiers() {
                        previous
                    } else {
                        base_type(&previous)
                            .without_type_modifiers_with_control(control)
                            .map_err(CommonTypeFailure::failed)?
                    },
                )
            }
            (Some(previous), Some(ty)) if self.same_type && *previous == *ty => Some(previous),
            (Some(previous), Some(ty)) => {
                self.same_type = false;
                Some(select_pair_with_control(
                    base_type(&previous),
                    base_type(ty),
                    control,
                )?)
            }
        };
        Ok(())
    }

    pub(in crate::type_resolution) fn finish(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<ColumnType>, SQLError> {
        control.check()?;
        self.selected.map_or_else(
            || {
                ColumnType::Text
                    .clone_with_control(control)
                    .map_err(Into::into)
            },
            Ok,
        )
    }
}

/// `select_common_type` with the construct's exact type and coercion diagnostics.
pub(crate) fn select_common_input_type_in_with_control<'a>(
    context: CommonTypeContext,
    types: impl Iterator<Item = Option<&'a ColumnType>> + Clone,
    control: &ProductionControl<'_>,
) -> Result<Produced<ColumnType>, SQLError> {
    // Inspect borrowed identities before copying so a later unknown does not charge discarded domain names.
    let mut selection = CommonInputType::from_inputs(types.clone(), control)?;
    for ty in types {
        selection.include(context, ty, control)?;
    }
    selection.finish(control)
}

/// SQL `unknown` inputs participate in the exact-type check. Incompatible known types return `None`; resource errors remain errors.
pub(crate) fn select_common_input_type_with_control(
    types: &[Option<&ColumnType>],
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    let mut selection = CommonInputType::from_inputs(types.iter().copied(), control)?;
    for ty in types {
        match selection.select(*ty, control) {
            Ok(()) => {}
            Err(CommonTypeFailure::Failed(error))
                if matches!(error.sqlstate(), Some("53200" | "57014")) =>
            {
                return Err(*error);
            }
            Err(_) => return Ok(None),
        }
    }
    selection.finish(control).map(Some)
}

/// Select a common type without production limits, returning `None` for incompatible known types.
pub fn select_common_input_type(
    types: &[Option<&ColumnType>],
) -> Result<Option<ColumnType>, SQLError> {
    select_common_input_type_with_control(types, &ProductionControl::uncontrolled()).map(
        |selected| {
            selected.map(|value| {
                value
                    .into_uncontrolled()
                    .expect("ordinary common type has no reservation")
            })
        },
    )
}
