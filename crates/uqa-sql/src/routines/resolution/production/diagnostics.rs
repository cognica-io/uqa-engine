//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine diagnostics admit dynamic signature and type formatting before crossing the error boundary.

use super::super::RoutineCallKind;
use crate::{
    ast::{CreateFunction, FunctionBinding, RoutineDefaultType},
    routines::declaration::RoutineTypeCatalog,
    type_resolution::{RoutineCallDescriptor, RoutineSignatureMatchError},
    SQLError,
};
use uqa_core::memory::{Produced, ProductionControl, ProductionString};

pub(in crate::routines::resolution) fn arguments(
    call: RoutineCallDescriptor<'_>,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, SQLError> {
    let mut output = ProductionString::new(*control);
    for (index, ty) in call.argument_types.iter().enumerate() {
        control.check()?;
        if index != 0 {
            output.push_str(", ")?;
        }
        if let Some(name) = call.argument_names.get(index).and_then(Option::as_deref) {
            output.push_str(name)?;
            output.push_str(" => ")?;
        }
        if let Some(ty) = ty {
            output.push_str(&ty.regtype_name_with_control(control)?)?;
        } else {
            output.push_str("unknown")?;
        }
    }
    Ok(output.finish()?)
}

pub(in crate::routines::resolution) fn kind(
    name: &str,
    call: RoutineCallDescriptor<'_>,
    expected: RoutineCallKind,
    definition: &CreateFunction,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    if definition.is_procedure == expected.is_procedure() {
        return Ok(());
    }
    let arguments = arguments(call, control)?;
    let (suffix, hint) = if definition.is_procedure {
        ("is a procedure", "To call a procedure, use CALL.")
    } else {
        ("is not a procedure", "To call a function, use SELECT.")
    };
    let message = control.format(format_args!("{name}({}) {suffix}", &*arguments))?;
    let hint = control.copy_text(hint)?;
    let sqlstate = control.copy_text("42809")?;
    Err(SQLError::Diagnostic {
        sqlstate: sqlstate.into_parts().0,
        message: message.into_parts().0,
        detail: None,
        hint: Some(hint.into_parts().0),
    })
}

pub(in crate::routines::resolution) fn bound(
    kind: RoutineCallKind,
    binding: &FunctionBinding,
    control: &ProductionControl<'_>,
) -> Result<SQLError, SQLError> {
    let mut signature = ProductionString::new(*control);
    for (index, ty) in binding.argument_types.iter().enumerate() {
        if index != 0 {
            signature.push_str(", ")?;
        }
        signature.push_str(ty)?;
    }
    let signature = signature.finish()?;
    let message = control.format(format_args!(
        "bound {} {}({}) does not exist",
        kind.name(),
        binding.name,
        &*signature
    ))?;
    let sqlstate = control.copy_text("42883")?;
    Ok(SQLError::Routine {
        sqlstate: sqlstate.into_parts().0,
        message: message.into_parts().0,
    })
}

pub(in crate::routines::resolution) fn resolution(
    kind: RoutineCallKind,
    sqlstate: &str,
    name: &str,
    call: RoutineCallDescriptor<'_>,
    suffix: &str,
    control: &ProductionControl<'_>,
) -> Result<SQLError, SQLError> {
    let arguments = arguments(call, control)?;
    let message = control.format(format_args!(
        "{} {name}({}) {suffix}",
        kind.name(),
        &*arguments
    ))?;
    let hint = if sqlstate == "42725" {
        control.format(format_args!(
            "Could not choose a best candidate {}. You might need to add explicit type casts.",
            kind.name()
        ))?
    } else {
        control.format(format_args!("No {} matches the given name and argument types. You might need to add explicit type casts.", kind.name()))?
    };
    let sqlstate = control.copy_text(sqlstate)?;
    Ok(SQLError::Diagnostic {
        sqlstate: sqlstate.into_parts().0,
        message: message.into_parts().0,
        detail: None,
        hint: Some(hint.into_parts().0),
    })
}

pub(in crate::routines::resolution) fn default_type(
    catalog: &dyn RoutineTypeCatalog,
    error: &RoutineSignatureMatchError,
    control: &ProductionControl<'_>,
) -> Result<SQLError, SQLError> {
    use RoutineSignatureMatchError as E;
    control.check()?;
    let format = |actual: &RoutineDefaultType| match actual {
        RoutineDefaultType::Concrete(ty) => catalog.format_type_with_control(ty, control),
        RoutineDefaultType::Polymorphic(name) => control.copy_text(name).map_err(Into::into),
    };
    let mut detail = None;
    let message = match error {
        E::InconsistentDefault {
            declared,
            first,
            second,
        } => {
            let first = format(first)?;
            let second = format(second)?;
            detail = Some(control.format(format_args!("{} versus {}", &*first, &*second))?);
            control.format(format_args!(
                "arguments declared \"{declared}\" are not all alike"
            ))?
        }
        E::InconsistentPolymorphicTypes {
            first_declared,
            second_declared,
            first,
            second,
        } => {
            let first = format(first)?;
            let second = format(second)?;
            detail = Some(control.format(format_args!("{} versus {}", &*first, &*second))?);
            control.format(format_args!("argument declared {first_declared} is not consistent with argument declared {second_declared}"))?
        }
        E::InvalidPolymorphicActual { declared, actual } => {
            let kind = if declared.ends_with("multirange") {
                "a multirange type"
            } else if declared.ends_with("range") {
                "a range type"
            } else {
                "an array"
            };
            let actual = format(actual)?;
            control.format(format_args!(
                "argument declared {declared} is not {kind} but type {}",
                &*actual
            ))?
        }
        E::IncompatibleDefaultTypes { first, second } => {
            let first = catalog.format_type_with_control(first, control)?;
            let second = catalog.format_type_with_control(second, control)?;
            control.format(format_args!(
                "argument types {} and {} cannot be matched",
                &*first, &*second
            ))?
        }
        E::IncompatibleRangeSubtype {
            declared,
            range,
            common,
        } => {
            let range = catalog.format_type_with_control(range, control)?;
            let common = catalog.format_type_with_control(common, control)?;
            control.format(format_args!(
                "{declared} type {} does not match anycompatible type {}",
                &*range, &*common
            ))?
        }
        E::InvalidPolymorphicElement { declared, actual } => {
            let shape = if declared == "anyenum" {
                "not an enum type"
            } else {
                "an array type"
            };
            let actual = catalog.format_type_with_control(actual, control)?;
            control.format(format_args!(
                "type matched to {declared} is {shape}: {}",
                &*actual
            ))?
        }
        E::IndeterminatePolymorphicArgument { declared } => control.format(format_args!(
            "could not determine polymorphic type {declared} because input has type unknown"
        ))?,
        E::IndeterminateArrayElement => {
            control.copy_text("cannot determine element type of \"anyarray\" argument")?
        }
        E::IndeterminatePolymorphicType { .. } => control
            .copy_text("could not determine polymorphic type because input has type unknown")?,
        E::InvalidVariadicSignature { reason } => control.copy_text(reason)?,
    };
    let sqlstate = control.copy_text(error.sqlstate())?;
    Ok(SQLError::Diagnostic {
        sqlstate: sqlstate.into_parts().0,
        message: message.into_parts().0,
        detail: detail.map(|value| value.into_parts().0),
        hint: None,
    })
}

pub(in crate::routines::resolution) fn signature(
    error: &RoutineSignatureMatchError,
    control: &ProductionControl<'_>,
) -> Result<SQLError, SQLError> {
    let message = match error {
        RoutineSignatureMatchError::InvalidVariadicSignature { reason } => {
            control.copy_text(reason)?
        }
        RoutineSignatureMatchError::IndeterminatePolymorphicType { .. } => control
            .copy_text("could not determine polymorphic type because input has type unknown")?,
        _ => unreachable!("default consistency is checked after candidate selection"),
    };
    let sqlstate = control.copy_text(error.sqlstate())?;
    Ok(SQLError::Routine {
        sqlstate: sqlstate.into_parts().0,
        message: message.into_parts().0,
    })
}
