//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain the declared search path, settings and explicit execution privileges before binding.

use super::{Copying, CreateFunction, Result, SyntaxCopy};
use crate::ast::RoutineAclEntry;
use uqa_core::memory::{BudgetedVec, Produced, ProductionControl, ProductionString};

/// Both the inline syntax copier and a retained declaration construct the same list item.
pub(in crate::plan::copy) trait Item: Sized {
    fn start(&self, copying: &mut Copying<'_>) -> Result<Self>;
    fn fill(&self, value: &mut Self, copying: &mut Copying<'_>) -> Result<bool>;
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
        if value.0.len() < self.0.len() {
            Ok(copy_text(&self.0, &mut value.0, copying)? && self.1.is_empty())
        } else {
            copy_text(&self.1, &mut value.1, copying)
        }
    }
}

impl Item for RoutineAclEntry {
    fn start(&self, copying: &mut Copying<'_>) -> Result<Self> {
        self.copy_syntax(copying)
    }

    fn fill(&self, _: &mut Self, _: &mut Copying<'_>) -> Result<bool> {
        // The two role identities and the grant option have fixed size.
        Ok(true)
    }
}

#[derive(Clone, Copy)]
pub(in crate::plan::copy) enum Lists {
    Inline,
    Retained,
}

impl Lists {
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
    Settings,
    Grants,
    Complete,
}

#[derive(Default)]
pub(super) struct Header {
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
                    List::Settings
                }
                List::Settings if self.position == source.config.len() => List::Grants,
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

    /// Short declarations complete in the constructor. Each unit visits one grant or at most one UTF-8 chunk, even when a single setting has a large value.
    pub(super) fn copy_window(
        &mut self,
        source: &CreateFunction,
        output: &mut CreateFunction,
        copying: &mut Copying<'_>,
    ) -> Result<()> {
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
                List::Settings => advance(&source.config, &mut output.config, i, copying)?,
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

fn advance<T: Item>(
    source: &[T],
    values: &mut Vec<T>,
    i: usize,
    copying: &mut Copying<'_>,
) -> Result<bool> {
    if values.capacity() == 0 {
        *values = Lists::reserve(source.len(), copying)?;
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
