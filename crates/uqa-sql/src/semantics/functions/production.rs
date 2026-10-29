//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Normalized function identities borrow their source; case folding owns one admitted buffer.

use uqa_core::{
    memory::{Produced, ProductionControl},
    ValueRetentionError,
};

pub enum FunctionName<'a> {
    Borrowed(&'a str),
    Owned(Produced<String>),
}

#[cfg(test)]
mod tests;

impl FunctionName<'_> {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Borrowed(name) => name,
            Self::Owned(name) => name,
        }
    }
    pub fn reserved_bytes(&self) -> usize {
        match self {
            Self::Borrowed(_) => 0,
            Self::Owned(name) => name.reserved_bytes(),
        }
    }
}

impl std::ops::Deref for FunctionName<'_> {
    type Target = str;
    fn deref(&self) -> &str {
        self.as_str()
    }
}

pub fn lowercase_function_name_with_control<'a>(
    name: &'a str,
    control: &ProductionControl<'_>,
) -> Result<FunctionName<'a>, ValueRetentionError> {
    control.check()?;
    if !name.bytes().any(|byte| byte.is_ascii_uppercase()) {
        return Ok(FunctionName::Borrowed(name));
    }
    let (mut name, memory) = control.copy_text(name)?.into_parts();
    name.make_ascii_lowercase();
    Ok(FunctionName::Owned(control.finish(name, memory)?))
}

pub fn builtin_function_dispatch_name_with_control<'a>(
    name: &'a str,
    control: &ProductionControl<'_>,
) -> Result<FunctionName<'a>, ValueRetentionError> {
    let lower = lowercase_function_name_with_control(name, control)?;
    // The shared selector only borrows when the input has already been folded.
    let selected_length = match super::builtin_function_dispatch_name(&lower) {
        std::borrow::Cow::Borrowed(selected) => selected.len(),
        std::borrow::Cow::Owned(_) => unreachable!("normalized function name"),
    };
    let offset = lower.len() - selected_length;
    match lower {
        FunctionName::Borrowed(name) => Ok(FunctionName::Borrowed(&name[offset..])),
        FunctionName::Owned(name) => {
            let (mut name, memory) = name.into_parts();
            name.drain(..offset);
            Ok(FunctionName::Owned(control.finish(name, memory)?))
        }
    }
}
