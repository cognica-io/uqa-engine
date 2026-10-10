//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Preserve the original write order until a rebase produces related-key replacements.

use std::cell::RefCell;

use crate::mvcc::{
    commit::PreparedWritesBuilder, PreparedRecordCommit, PreparedRecordWrite, PrivateRecordChanges,
    VersionResult,
};
use crate::read_control::StorageReadControl;

enum Target {
    Ordered(PreparedWritesBuilder),
    Merged(PrivateRecordChanges),
}

/// Canonical and structurally fenced writes retain their input order, so a spilled batch can be written directly once. Derived related-key effects switch to the ordinary private overlay before their first replacement, preserving the original sequence of overwrites and write conditions.
pub(super) struct ResolvedChanges(RefCell<Option<Target>>);

impl ResolvedChanges {
    pub(super) fn new(
        original: &PreparedRecordCommit,
        control: &StorageReadControl,
    ) -> VersionResult<Self> {
        Ok(Self(RefCell::new(Some(Target::Ordered(
            PreparedWritesBuilder::like(original, control)?,
        )))))
    }

    pub(super) fn preserve(
        &self,
        write: PreparedRecordWrite,
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        match self
            .0
            .borrow_mut()
            .as_mut()
            .expect("an active occurrence resolution")
        {
            Target::Ordered(builder) => builder.push(write, control),
            Target::Merged(changes) => changes.apply_owned(&[write], control),
        }
    }

    pub(super) fn replace(
        &self,
        write: PreparedRecordWrite,
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        let mut target = self.0.borrow_mut();
        if matches!(*target, Some(Target::Ordered(_))) {
            let Some(Target::Ordered(builder)) = target.take() else {
                unreachable!()
            };
            *target = Some(Target::Merged(builder.finish_changes(None, control)?));
        }
        let Some(Target::Merged(changes)) = &*target else {
            unreachable!()
        };
        changes.apply_owned(&[write], control)
    }

    pub(super) fn finish(
        self,
        control: &StorageReadControl,
    ) -> VersionResult<PreparedRecordCommit> {
        match self
            .0
            .into_inner()
            .expect("an active occurrence resolution")
        {
            Target::Ordered(builder) => builder.finish(control),
            Target::Merged(changes) => changes.prepare(control),
        }
    }
}
