//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capture stored declaration fields in order before binding their parameters and body.

use super::{CopyPurpose, Copying, CreateFunction, Result, SyntaxCopy};
use crate::ast::{
    FunctionReturns, RoutineAclEntry, RoutineAttributeClause, RoutineConfigAction,
    RoutineTransformType,
};
use crate::plan::copy::HeaderText;
use uqa_core::memory::{BudgetedVec, Produced, ProductionControl, ProductionString};

/// Both the inline syntax copier and a retained declaration construct the same list item.
pub(in crate::plan::copy) trait Item: Sized {
    fn start(&self, copying: &mut Copying<'_>) -> Result<Self>;
    fn fill(&self, _: &mut Self, _: &mut Copying<'_>) -> Result<bool> {
        Ok(true)
    }
}

impl Item for String {
    fn start(&self, _: &mut Copying<'_>) -> Result<Self> {
        Ok(Self::new())
    }

    fn fill(&self, value: &mut Self, copying: &mut Copying<'_>) -> Result<bool> {
        copy_text(self, value, copying)
    }
}

impl Item for (String, String) {
    fn start(&self, _: &mut Copying<'_>) -> Result<Self> {
        Ok(Self::default())
    }

    fn fill(&self, value: &mut Self, copying: &mut Copying<'_>) -> Result<bool> {
        copy_pair(&self.0, &self.1, &mut value.0, &mut value.1, copying)
    }
}

impl Item for RoutineAclEntry {
    fn start(&self, copying: &mut Copying<'_>) -> Result<Self> {
        self.copy_syntax(copying)
    }
}

impl Item for RoutineAttributeClause {
    fn start(&self, copying: &mut Copying<'_>) -> Result<Self> {
        self.copy_syntax(copying)
    }
}

impl Item for RoutineConfigAction {
    fn start(&self, copying: &mut Copying<'_>) -> Result<Self> {
        HeaderCopy::Retained.project(self, copying, HeaderText::WhenSelected)
    }

    fn fill(&self, output: &mut Self, copying: &mut Copying<'_>) -> Result<bool> {
        match (self, output) {
            (
                Self::Set { name, value },
                Self::Set {
                    name: key,
                    value: setting,
                },
            ) => copy_pair(name, value, key, setting, copying),
            (Self::FromCurrent { name }, Self::FromCurrent { name: key })
            | (Self::Reset { name }, Self::Reset { name: key }) => copy_text(name, key, copying),
            (Self::ResetAll, Self::ResetAll) => Ok(true),
            _ => unreachable!("the stored configuration action retains its declared shape"),
        }
    }
}

impl Item for RoutineTransformType {
    fn start(&self, copying: &mut Copying<'_>) -> Result<Self> {
        HeaderCopy::Retained.project(self, copying, HeaderText::WhenSelected)
    }

    fn fill(&self, output: &mut Self, copying: &mut Copying<'_>) -> Result<bool> {
        copy_pair(
            &self.type_name,
            &self.written,
            &mut output.type_name,
            &mut output.written,
            copying,
        )
    }
}

#[derive(Clone, Copy)]
pub(in crate::plan::copy) enum HeaderCopy {
    Inline,
    Retained,
}

impl HeaderCopy {
    /// Reuse original syntax constructors for discriminants and optional fields. Fixed prefix strings reserve before the body. Later entries reserve each string in its declared order.
    pub(in crate::plan::copy) fn project<T: SyntaxCopy>(
        self,
        source: &T,
        copying: &mut Copying<'_>,
        timing: HeaderText,
    ) -> Result<T> {
        match self {
            Self::Inline => source.copy_syntax(copying),
            Self::Retained => {
                let mut shape =
                    Copying::capture(copying.control, CopyPurpose::StoredHeader(timing));
                let value = source.copy_syntax(&mut shape)?;
                debug_assert_eq!(shape.retained_children, 0);
                Ok(copying.take(shape.finish(value)?))
            }
        }
    }

    pub(in crate::plan::copy) fn copy<T: Item>(
        self,
        source: &[T],
        copying: &mut Copying<'_>,
    ) -> Result<Vec<T>> {
        match self {
            Self::Inline => copying.map(source.iter(), |child, item| {
                let mut value = item.start(child)?;
                while !item.fill(&mut value, child)? {
                    child.check()?;
                }
                Ok(value)
            }),
            Self::Retained => Ok(Vec::new()),
        }
    }

    pub(super) fn reserve<T>(len: usize, copying: &mut Copying<'_>) -> Result<Vec<T>> {
        let mut slots = BudgetedVec::new(copying.control.budget().expect("stored definition"));
        slots.reserve(len)?;
        let (slots, lease) = slots.into_parts();
        copying.absorb(Some(lease));
        Ok(slots)
    }
}

#[derive(Clone, Copy, Default)]
enum List {
    #[default]
    SearchPath,
    Support,
    Settings,
    Actions,
    Clauses,
    InvalidParallel,
    Transforms,
    Grants,
    Complete,
}

#[derive(Default)]
pub(super) struct Header {
    prefix_complete: bool,
    list: List,
    position: usize,
}

impl Header {
    pub(super) const fn complete(&self) -> bool {
        matches!(self.list, List::Complete)
    }

    fn select(&mut self, source: &CreateFunction) {
        loop {
            self.list = match self.list {
                List::SearchPath if self.position == source.creation_search_path.len() => {
                    List::Support
                }
                List::Support
                    if self.position != 0
                        || source.support.as_deref().is_none_or(str::is_empty) =>
                {
                    List::Settings
                }
                List::Settings if self.position == source.config.len() => List::Actions,
                List::Actions if self.position == source.config_actions.len() => List::Clauses,
                List::Clauses if self.position == source.attribute_clauses.clauses.len() => {
                    List::InvalidParallel
                }
                List::InvalidParallel
                    if self.position != 0
                        || source
                            .attribute_clauses
                            .invalid_parallel
                            .as_deref()
                            .is_none_or(str::is_empty) =>
                {
                    List::Transforms
                }
                List::Transforms
                    if self.position == source.attribute_clauses.transform_types.len() =>
                {
                    List::Grants
                }
                List::Grants
                    if self.position == source.execute_acl.as_deref().unwrap_or_default().len() =>
                {
                    List::Complete
                }
                _ => return,
            };
            self.position = 0;
        }
    }

    /// Advance the fixed declaration fields in order and up to eight subsequent entries or text chunks.
    pub(super) fn copy_window(
        &mut self,
        source: &CreateFunction,
        output: &mut CreateFunction,
        copying: &mut Copying<'_>,
    ) -> Result<()> {
        if !self.prefix_complete {
            self.prefix_complete = copy_prefix(source, output, copying)?;
            if !self.prefix_complete {
                return copying.check();
            }
        }
        self.select(source);
        for _ in 0..8 {
            copying.check()?;
            let i = self.position;
            let finished = match self.list {
                List::SearchPath => advance(
                    &source.creation_search_path,
                    &mut output.creation_search_path,
                    i,
                    copying,
                )?,
                List::Support => {
                    copy_optional(source.support.as_deref(), output.support.as_mut(), copying)?
                }
                List::Settings => advance(&source.config, &mut output.config, i, copying)?,
                List::Actions => advance(
                    &source.config_actions,
                    &mut output.config_actions,
                    i,
                    copying,
                )?,
                List::Clauses => advance(
                    &source.attribute_clauses.clauses,
                    &mut output.attribute_clauses.clauses,
                    i,
                    copying,
                )?,
                List::InvalidParallel => copy_optional(
                    source.attribute_clauses.invalid_parallel.as_deref(),
                    output.attribute_clauses.invalid_parallel.as_mut(),
                    copying,
                )?,
                List::Transforms => advance(
                    &source.attribute_clauses.transform_types,
                    &mut output.attribute_clauses.transform_types,
                    i,
                    copying,
                )?,
                List::Grants => advance(
                    source.execute_acl.as_deref().expect("selected grant"),
                    output.execute_acl.as_mut().expect("explicit ACL"),
                    i,
                    copying,
                )?,
                List::Complete => break,
            };
            if finished {
                self.position += 1;
                self.select(source);
            }
        }
        copying.check()
    }
}

fn copy_prefix(
    source: &CreateFunction,
    output: &mut CreateFunction,
    copying: &mut Copying<'_>,
) -> Result<bool> {
    if !copy_text(&source.name, &mut output.name, copying)? {
        return Ok(false);
    }
    match (&source.returns, &mut output.returns) {
        (FunctionReturns::Scalar { type_name }, FunctionReturns::Scalar { type_name: value })
        | (FunctionReturns::SetOf { type_name }, FunctionReturns::SetOf { type_name: value }) => {
            if !copy_text(type_name, value, copying)? {
                return Ok(false);
            }
        }
        (FunctionReturns::None, FunctionReturns::None)
        | (FunctionReturns::Table, FunctionReturns::Table) => {}
        _ => unreachable!("the stored result retains its declared shape"),
    }
    if let Some(original) = &source.return_type_reference {
        let value = output
            .return_type_reference
            .as_mut()
            .expect("stored column type reference");
        if !copy_optional(original.schema.as_deref(), value.schema.as_mut(), copying)?
            || !copy_text(&original.relation, &mut value.relation, copying)?
            || !copy_text(&original.column, &mut value.column, copying)?
        {
            return Ok(false);
        }
    }
    Ok(copy_optional(
        source.return_written_type.as_deref(),
        output.return_written_type.as_mut(),
        copying,
    )? && copy_text(&source.language, &mut output.language, copying)?)
}

fn copy_optional(
    source: Option<&str>,
    output: Option<&mut String>,
    copying: &mut Copying<'_>,
) -> Result<bool> {
    match (source, output) {
        (Some(source), Some(output)) => copy_text(source, output, copying),
        (None, None) => Ok(true),
        _ => unreachable!("the stored field retains its presence"),
    }
}

fn copy_pair(
    first: &str,
    second: &str,
    key: &mut String,
    value: &mut String,
    copying: &mut Copying<'_>,
) -> Result<bool> {
    if key.len() < first.len() {
        Ok(copy_text(first, key, copying)? && second.is_empty())
    } else {
        copy_text(second, value, copying)
    }
}

fn advance<T: Item>(
    source: &[T],
    values: &mut Vec<T>,
    i: usize,
    copying: &mut Copying<'_>,
) -> Result<bool> {
    if values.capacity() == 0 {
        *values = HeaderCopy::reserve(source.len(), copying)?;
    }
    let item = &source[i];
    if values.len() == i {
        debug_assert!(i < values.capacity());
        values.push(item.start(copying)?);
    }
    item.fill(&mut values[i], copying)
}

fn copy_text(source: &str, output: &mut String, copying: &mut Copying<'_>) -> Result<bool> {
    if output.len() < source.len() {
        if output.capacity() == 0 {
            *output = copying.take(start_text(source, &copying.control)?);
        } else {
            continue_text(source, output);
        }
    }
    Ok(output.len() == source.len())
}

pub(super) fn start_text(
    source: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>> {
    let mut text = ProductionString::new(*control);
    text.reserve(source.len())?;
    text.push_str_chunk(source)?;
    Ok(text.finish()?)
}

pub(super) fn continue_text(source: &str, output: &mut String) {
    // start_text admitted this allocation at its complete size.
    debug_assert!(output.capacity() >= source.len());
    output.push_str(ProductionString::first_chunk(&source[output.len()..]));
}
