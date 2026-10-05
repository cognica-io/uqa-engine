//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The objects a `DROP` deletes, found and reported as `findDependentObjects` and `reportDependentObjects` do.

use super::{DependencyGraph, DependencyKind, ObjectAddress};
use crate::SQLError;
use std::fmt::Write as _;

/// `DEPFLAG_*`: how the search reached an object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DeletionFlags(u16);

impl DeletionFlags {
    /// An object the command names.
    pub const ORIGINAL: Self = Self(0x0001);
    pub const NORMAL: Self = Self(0x0002);
    pub const AUTO: Self = Self(0x0004);
    pub const INTERNAL: Self = Self(0x0008);
    pub const PARTITION: Self = Self(0x0010);
    pub const EXTENSION: Self = Self(0x0020);
    /// Reached from an object that is part of it, which redirected the search to its owner.
    pub const REVERSE: Self = Self(0x0040);
    /// The object has a partition dependency.
    pub const IS_PART: Self = Self(0x0080);
    /// A column of a relation that is deleted as a whole.
    pub const SUBOBJECT: Self = Self(0x0100);

    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    const fn reached_by(kind: DependencyKind) -> Self {
        match kind {
            DependencyKind::Normal => Self::NORMAL,
            DependencyKind::Auto | DependencyKind::AutoExtension => Self::AUTO,
            DependencyKind::Internal => Self::INTERNAL,
            DependencyKind::PartitionPrimary | DependencyKind::PartitionSecondary => {
                Self::PARTITION
            }
            DependencyKind::Extension => Self::EXTENSION,
        }
    }

    /// Objects reached through these dependencies are deleted without `CASCADE` and not reported.
    const fn deleted_silently(self) -> bool {
        self.contains(Self::AUTO)
            || self.contains(Self::INTERNAL)
            || self.contains(Self::PARTITION)
            || self.contains(Self::EXTENSION)
    }
}

/// One object to delete, with how it was reached and the object whose deletion requires it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeletionTarget {
    pub object: ObjectAddress,
    pub flags: DeletionFlags,
    pub dependee: Option<ObjectAddress>,
}

/// The `NOTICE` a cascading `DROP` reports: one line for a single object, or a count with the list in the detail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CascadeNotice {
    pub message: String,
    pub detail: Option<String>,
}

struct StackEntry {
    object: ObjectAddress,
    flags: DeletionFlags,
}

/// The objects to delete in deletion order: every dependent before the object it depends on.
#[derive(Debug, Clone, Default)]
pub struct DeletionTargets {
    targets: Vec<DeletionTarget>,
}

/// `MAX_REPORTED_DEPS`: the detail lists at most this many objects.
const MAX_REPORTED_DEPENDENTS: usize = 100;

impl DeletionTargets {
    /// `performMultipleDeletions`' search: the named objects and everything that depends on them. An object that is internal to another named object is deleted with its owner; one that is internal to an unnamed object reports `2BP01` as `findDependentObjects` does.
    pub fn collect(
        graph: &DependencyGraph,
        originals: &[ObjectAddress],
        describe: super::Describe<'_>,
    ) -> Result<Self, SQLError> {
        let mut search = Self::default();
        for original in originals {
            search.find(
                graph,
                *original,
                DeletionFlags::ORIGINAL,
                &mut Vec::new(),
                originals,
                describe,
            )?;
        }
        Ok(search)
    }

    pub fn targets(&self) -> &[DeletionTarget] {
        &self.targets
    }

    fn find(
        &mut self,
        graph: &DependencyGraph,
        object: ObjectAddress,
        mut flags: DeletionFlags,
        stack: &mut Vec<StackEntry>,
        pending: &[ObjectAddress],
        describe: super::Describe<'_>,
    ) -> Result<(), SQLError> {
        if stack_present_add_flags(object, flags, stack) || self.present_add_flags(object, flags) {
            return Ok(());
        }
        let mut owner = None;
        let mut partition = None;
        let references = graph.references_of(object).copied().collect::<Vec<_>>();
        for dependency in references {
            let other = dependency.referenced;
            // A column's dependency on its own relation is implicit.
            if other.same_object(object) && object.sub_id == 0 {
                continue;
            }
            match dependency.kind {
                DependencyKind::Normal | DependencyKind::Auto | DependencyKind::AutoExtension => {}
                DependencyKind::Extension | DependencyKind::Internal => {
                    if stack.is_empty() {
                        if pending.iter().any(|named| {
                            named.same_object(other)
                                && (named.sub_id == other.sub_id || named.sub_id == 0)
                        }) {
                            // The owner is named too; deleting it deletes this object.
                            return Ok(());
                        }
                        if owner.is_none() || dependency.kind == DependencyKind::Extension {
                            owner = Some(other);
                        }
                        continue;
                    }
                    if stack_present_add_flags(other, DeletionFlags::default(), stack) {
                        continue;
                    }
                    // Delete the owner instead; this object is deleted when the search returns to it from the owner.
                    self.find(
                        graph,
                        other,
                        DeletionFlags::REVERSE,
                        stack,
                        pending,
                        describe,
                    )?;
                    if !self.present_add_flags(object, flags) {
                        return Err(SQLError::Internal(format!(
                            "deletion of owning object {} failed to delete {}",
                            describe_or_address(describe, other)?,
                            describe_or_address(describe, object)?
                        )));
                    }
                    return Ok(());
                }
                DependencyKind::PartitionPrimary => {
                    flags = flags.union(DeletionFlags::IS_PART);
                    partition = Some(other);
                }
                DependencyKind::PartitionSecondary => {
                    if !flags.contains(DeletionFlags::IS_PART) {
                        partition = Some(other);
                    }
                    flags = flags.union(DeletionFlags::IS_PART);
                }
            }
        }
        if let Some(owner) = owner {
            let required_by = describe_or_address(describe, partition.unwrap_or(owner))?;
            return Err(SQLError::Diagnostic {
                sqlstate: "2BP01".into(),
                message: format!(
                    "cannot drop {} because {required_by} requires it",
                    describe_or_address(describe, object)?
                ),
                detail: None,
                hint: Some(format!("You can drop {required_by} instead.")),
            });
        }
        let mut dependents = graph
            .dependents_of(object)
            .filter(|dependency| !(dependency.dependent.same_object(object) && object.sub_id == 0))
            .map(|dependency| {
                (
                    dependency.dependent,
                    DeletionFlags::reached_by(dependency.kind),
                )
            })
            .collect::<Vec<_>>();
        dependents.sort_by(|left, right| left.0.deletion_order(&right.0));
        stack.push(StackEntry { object, flags });
        for (dependent, reached) in dependents {
            self.find(graph, dependent, reached, stack, pending, describe)?;
        }
        let entry = stack.pop().expect("the pushed stack entry");
        let dependee = if entry.flags.contains(DeletionFlags::IS_PART) {
            partition
        } else {
            stack.last().map(|parent| parent.object)
        };
        self.targets.push(DeletionTarget {
            object,
            flags: entry.flags,
            dependee,
        });
        Ok(())
    }

    /// `object_address_present_add_flags`: whether the object, or the whole relation of a column, is already a target, merging the new flags into it. A whole relation marks its columns already found as subobjects, which are not reported.
    fn present_add_flags(&mut self, object: ObjectAddress, flags: DeletionFlags) -> bool {
        let mut present = false;
        for target in self.targets.iter_mut().rev() {
            if !target.object.same_object(object) {
                continue;
            }
            if target.object.sub_id == object.sub_id {
                target.flags = target.flags.union(flags);
                present = true;
            } else if target.object.sub_id == 0 {
                present = true;
            } else if object.sub_id == 0 && flags != DeletionFlags::default() {
                target.flags = target.flags.union(flags).union(DeletionFlags::SUBOBJECT);
            }
        }
        present
    }

    /// `reportDependentObjects`: reject a `RESTRICT` drop that would delete an object reached through a normal dependency, or describe what a cascading drop deletes. `original` names the single object the command drops.
    pub fn report(
        &self,
        cascade: bool,
        original: Option<ObjectAddress>,
        describe: super::Describe<'_>,
    ) -> Result<Option<CascadeNotice>, SQLError> {
        for target in &self.targets {
            if target.flags.contains(DeletionFlags::IS_PART)
                && !target.flags.contains(DeletionFlags::PARTITION)
            {
                let required_by = target
                    .dependee
                    .map(|dependee| describe_or_address(describe, dependee))
                    .transpose()?
                    .unwrap_or_default();
                return Err(SQLError::Diagnostic {
                    sqlstate: "2BP01".into(),
                    message: format!(
                        "cannot drop {} because {required_by} requires it",
                        describe_or_address(describe, target.object)?
                    ),
                    detail: None,
                    hint: Some(format!("You can drop {required_by} instead.")),
                });
            }
        }
        let mut lines = Vec::new();
        let mut unreported = 0_usize;
        let mut allowed = true;
        for target in self.targets.iter().rev() {
            if target.flags.contains(DeletionFlags::ORIGINAL)
                || target.flags.contains(DeletionFlags::SUBOBJECT)
            {
                continue;
            }
            let Some(description) = describe(target.object)? else {
                continue;
            };
            if target.flags.deleted_silently() {
                continue;
            }
            let line = if cascade {
                Some(format!("drop cascades to {description}"))
            } else {
                allowed = false;
                match target.dependee {
                    Some(dependee) => describe(dependee)?
                        .map(|dependee| format!("{description} depends on {dependee}")),
                    None => None,
                }
            };
            match line {
                Some(line) if lines.len() < MAX_REPORTED_DEPENDENTS => lines.push(line),
                _ => unreported += 1,
            }
        }
        let reported = lines.len();
        let mut detail = lines.join("\n");
        if unreported > 0 {
            write!(
                detail,
                "\nand {unreported} other object{} (see server log for list)",
                if unreported == 1 { "" } else { "s" }
            )
            .expect("writing to a String cannot fail");
        }
        if !allowed {
            return Err(SQLError::Diagnostic {
                sqlstate: "2BP01".into(),
                message: match original {
                    Some(original) => format!(
                        "cannot drop {} because other objects depend on it",
                        describe_or_address(describe, original)?
                    ),
                    None => {
                        "cannot drop desired object(s) because other objects depend on them".into()
                    }
                },
                detail: Some(detail),
                hint: Some("Use DROP ... CASCADE to drop the dependent objects too.".into()),
            });
        }
        Ok(match reported {
            0 => None,
            1 => Some(CascadeNotice {
                message: detail,
                detail: None,
            }),
            _ => {
                let total = reported + unreported;
                Some(CascadeNotice {
                    message: format!(
                        "drop cascades to {total} other object{}",
                        if total == 1 { "" } else { "s" }
                    ),
                    detail: Some(detail),
                })
            }
        })
    }
}

/// `stack_address_present_add_flags`: whether the object, or the whole relation of a column, is being visited by an outer level of the search. Merging flags into the entry lets an inner level mark the object it redirected from.
fn stack_present_add_flags(
    object: ObjectAddress,
    flags: DeletionFlags,
    stack: &mut [StackEntry],
) -> bool {
    let mut present = false;
    for entry in stack.iter_mut() {
        if !entry.object.same_object(object) {
            continue;
        }
        if entry.object.sub_id == object.sub_id {
            entry.flags = entry.flags.union(flags);
            present = true;
        } else if entry.object.sub_id == 0 {
            present = true;
        } else if object.sub_id == 0 && flags != DeletionFlags::default() {
            entry.flags = entry.flags.union(flags).union(DeletionFlags::SUBOBJECT);
        }
    }
    present
}

fn describe_or_address(
    describe: super::Describe<'_>,
    object: ObjectAddress,
) -> Result<String, SQLError> {
    Ok(describe(object)?.unwrap_or_else(|| {
        format!(
            "object {} of class {} column {}",
            object.object_id, object.class_id, object.sub_id
        )
    }))
}

#[cfg(test)]
mod tests;
