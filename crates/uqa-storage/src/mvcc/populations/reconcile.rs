//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The same exact population transformation serves private mutation previews, command refresh and publication.

mod invalidation;

use super::{
    DiskANNPopulationHeader, DiskANNPopulationOrigin, DiskANNPopulationRecordLayout,
    OwnedPopulationMutation,
};
use crate::diskann_index::{pages::DiskANNOriginReader, DiskANNPopulationState, DiskANNQueryRead};
use crate::mvcc::{
    commit::RecordWriteKind, DatabaseId, MergedRecordSnapshot, PreparedRecordCommit,
    PreparedRecordWrite, PrivateRecordChanges, VersionError, VersionResult,
};
use crate::read_control::StorageReadControl;
use uqa_core::memory::{BudgetedMap, BudgetedVec};

type PopulationLifecycle<'a> = BudgetedMap<&'a [u8], &'a OwnedPopulationMutation>;
type RecordVisitor<'a> = dyn FnMut(&[u8], &[u8]) -> VersionResult<()> + 'a;

pub(super) struct Reconciliation<'a> {
    pub(super) before: &'a MergedRecordSnapshot,
    pub(super) after: &'a MergedRecordSnapshot,
    pub(super) layout: &'a dyn DiskANNPopulationRecordLayout,
    pub(super) history: DatabaseId,
    pub(super) control: &'a StorageReadControl,
    /// Final preparation preserves explicit structural writes. An ordinary mutation preview updates its preceding private header directly.
    pub(super) structural: Option<&'a super::structural::StructuralRecords<'a>>,
}

impl Reconciliation<'_> {
    pub(super) fn run(
        &self,
        origins: &[PreparedRecordWrite],
        lifecycle: &[OwnedPopulationMutation],
    ) -> VersionResult<PreparedRecordCommit> {
        let control = self.control;
        let output = PrivateRecordChanges::new(control.memory());
        let mut decoded = BudgetedVec::new(control.memory());
        for write in origins {
            control.check()?;
            if let Some(value) = write.value() {
                decoded.push(self.layout.origin(write.key(), value, control)?)?;
            }
        }
        let mut fields =
            BudgetedMap::<&[u8], BudgetedVec<&DiskANNPopulationOrigin>>::new(control.memory());
        for origin in decoded.iter() {
            control.check()?;
            if let Some(changes) = fields.get_mut(&*origin.field) {
                changes.push(origin)?;
            } else {
                let mut changes = BudgetedVec::new(control.memory());
                changes.push(origin)?;
                fields.insert(&origin.field, changes)?;
            }
        }
        let mut latest = BudgetedMap::new(control.memory());
        for operation in lifecycle {
            control.check()?;
            latest.insert(operation.key().bytes(), operation)?;
        }
        self.validate_invalidations(origins, &latest)?;
        for (&key, operation) in &latest {
            control.check()?;
            if self.structural_header(key)?.is_some_and(|write| {
                write.value().is_some()
                    || matches!(operation, OwnedPopulationMutation::Publish { .. })
            }) {
                continue;
            }
            match operation {
                OwnedPopulationMutation::Publish {
                    template, origins, ..
                } => self.publish(&output, key, template, origins)?,
                OwnedPopulationMutation::Retire { .. } => {
                    let prefix = self.layout.witness_prefix(key, control)?;
                    visit(self.after, &prefix, control, &mut |key, _| {
                        self.put(&output, key, None)
                    })?;
                    self.put(&output, key, None)?;
                }
            }
        }
        for (&field, changes) in &fields {
            let dimensions = changes[0].origin.dimensions();
            self.with_source(field, dimensions, self.after, &mut |current| {
                for change in changes.iter() {
                    control.check()?;
                    if change.origin.dimensions() != dimensions
                        || current.document_origin(change.document, control)? != Some(change.origin)
                    {
                        return Err(invalid(
                            "population input differs from complete canonical replacement",
                        ));
                    }
                }
                Ok(())
            })?;
            let prefix = self.layout.header_prefix(field, control)?;
            visit(self.after, &prefix, control, &mut |key, template| {
                if latest.contains_key(key) || self.structural_header(key)?.is_some() {
                    return Ok(());
                }
                let header = self.layout.header(key, template, control)?;
                if &*header.field != field || header.state.dimensions() != dimensions {
                    return Err(invalid(
                        "population header belongs to another canonical field",
                    ));
                }
                self.replace(&output, key, template, &header, changes)
            })?;
        }
        control.check()?;
        output.prepare(control)
    }

    fn publish(
        &self,
        output: &PrivateRecordChanges,
        key: &[u8],
        template: &[u8],
        origins: &DiskANNOriginReader,
    ) -> VersionResult<()> {
        let control = self.control;
        let header = self.layout.header(key, template, control)?;
        if header.state.generation() != origins.manifest().input().generation
            || header.state.dimensions() != origins.manifest().input().dimensions
        {
            return Err(invalid(
                "population publication differs from retained build",
            ));
        }
        self.with_source(
            &header.field,
            header.state.dimensions(),
            self.after,
            &mut |current| {
                let state =
                    DiskANNPopulationState::capture(current, origins, control, &mut |witness| {
                        let witness_key = self
                            .layout
                            .witness_key(key, witness.document(), control)
                            .map_err(VersionError::into_storage_error)?;
                        let bytes = self
                            .layout
                            .encode_witness(&witness_key, template, witness, control)
                            .map_err(VersionError::into_storage_error)?;
                        self.put(output, &witness_key, Some(&bytes))
                            .map_err(VersionError::into_storage_error)
                    })?;
                let bytes = self.layout.encode_header(key, template, state, control)?;
                self.put(output, key, Some(&bytes))
            },
        )
    }

    fn replace(
        &self,
        output: &PrivateRecordChanges,
        key: &[u8],
        template: &[u8],
        header: &DiskANNPopulationHeader,
        changes: &[&DiskANNPopulationOrigin],
    ) -> VersionResult<()> {
        let control = self.control;
        let mut state = header.state;
        self.with_source(
            &header.field,
            state.dimensions(),
            self.before,
            &mut |previous| {
                for change in changes {
                    control.check()?;
                    let origin = previous.document_origin(change.document, control)?;
                    let witness_key = self.layout.witness_key(key, change.document, control)?;
                    let witness = self.before.get(&witness_key, control)?;
                    let bytes = witness.as_ref().and_then(|row| row.value());
                    let previous = match (origin, bytes) {
                        (Some(origin), Some(bytes)) => Some((
                            origin,
                            self.layout.witness(
                                &witness_key,
                                bytes,
                                state,
                                change.document,
                                origin,
                                control,
                            )?,
                        )),
                        (None, None) => None,
                        _ => {
                            return Err(invalid(
                                "population witness is missing or has no current origin",
                            ))
                        }
                    };
                    let (next, witness) =
                        state.replaced(change.document, previous, change.origin)?;
                    let bytes =
                        self.layout
                            .encode_witness(&witness_key, template, witness, control)?;
                    self.put(output, &witness_key, Some(&bytes))?;
                    state = next;
                }
                Ok(())
            },
        )?;
        let bytes = self.layout.encode_header(key, template, state, control)?;
        self.put(output, key, Some(&bytes))
    }

    fn put(
        &self,
        output: &PrivateRecordChanges,
        key: &[u8],
        value: Option<&[u8]>,
    ) -> VersionResult<()> {
        let expected = self
            .before
            .metadata(key, self.control)?
            .and_then(|row| row.revision);
        let write = PreparedRecordWrite::copy_bytes(key, expected, value, self.control)?
            .with_kind(RecordWriteKind::DiskANNPopulationPreview);
        output.apply_owned(&[write], self.control)
    }

    fn structural_header(&self, key: &[u8]) -> VersionResult<Option<PreparedRecordWrite>> {
        match self.structural {
            Some(structural) => structural.get(key, self.control),
            None => Ok(None),
        }
    }

    fn with_source(
        &self,
        field: &[u8],
        dimensions: u32,
        view: &MergedRecordSnapshot,
        visit: &mut dyn FnMut(&dyn DiskANNQueryRead) -> VersionResult<()>,
    ) -> VersionResult<()> {
        let mut seen = false;
        let mut failed = false;
        self.layout.with_canonical(
            field,
            dimensions,
            view,
            self.history,
            self.control,
            &mut |source| {
                if seen || failed {
                    failed = true;
                    return Err(invalid("canonical population adapter invoked repeatedly"));
                }
                seen = true;
                let result = (|| {
                    source.check_control(self.control)?;
                    if source.dimensions() != dimensions {
                        return Err(invalid("canonical population adapter changed dimensions"));
                    }
                    visit(source)?;
                    source.check_control(self.control)?;
                    Ok(())
                })();
                failed = result.is_err();
                result
            },
        )?;
        if !seen || failed {
            return Err(invalid(
                "canonical population adapter suppressed completion",
            ));
        }
        self.control.check()?;
        Ok(())
    }
}

fn visit(
    view: &MergedRecordSnapshot,
    prefix: &[u8],
    control: &StorageReadControl,
    visit: &mut RecordVisitor<'_>,
) -> VersionResult<()> {
    let mut after = BudgetedVec::new(control.memory());
    loop {
        control.check()?;
        let page = view.scan(prefix, (!after.is_empty()).then_some(&*after), 64, control)?;
        let Some(last) = page.last() else {
            return Ok(());
        };
        after.clear();
        after.extend_from_slice(&last.key)?;
        for row in page.iter() {
            control.check()?;
            if let Some(value) = row.record.value() {
                visit(&row.key, value)?;
            }
        }
    }
}

fn invalid(message: &'static str) -> VersionError {
    VersionError::InvalidEncoding(message)
}
