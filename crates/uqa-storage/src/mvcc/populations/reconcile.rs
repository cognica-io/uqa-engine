//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The same exact population transformation serves private mutation previews, command refresh and publication.

mod invalidation;
#[cfg(test)]
mod tests;

use super::{
    DiskANNPopulationHeader, DiskANNPopulationOrigin, DiskANNPopulationRecordLayout,
    OwnedPopulationMutation,
};
use crate::diskann_index::{pages::DiskANNOriginReader, DiskANNPopulationState, DiskANNQueryRead};
use crate::mvcc::{
    commit::{PreparedLookup, RecordWriteKind},
    key::RecordKey,
    DatabaseId, MergedRecordSnapshot, PreparedRecordCommit, PreparedRecordWrite,
    PrivateRecordChanges, VersionError, VersionResult,
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
        origins: &PreparedRecordCommit,
        lifecycle: &[OwnedPopulationMutation],
    ) -> VersionResult<PreparedRecordCommit> {
        let control = self.control;
        let output = PrivateRecordChanges::new(control.memory());
        // Only distinct field identities remain resident. Replay the sealed origin records one at a time; neither canonical vectors nor all decoded origins are retained here.
        let mut fields = BudgetedMap::<RecordKey, u32>::new(control.memory());
        self.visit_origins(origins, &mut |origin| {
            if let Some(dimensions) = fields.get(&*origin.field) {
                if *dimensions != origin.origin.dimensions() {
                    return Err(invalid("population inputs disagree on field dimensions"));
                }
            } else {
                fields.insert(
                    RecordKey::new(&origin.field, control.memory())?,
                    origin.origin.dimensions(),
                )?;
            }
            Ok(())
        })?;
        // Final preparation already indexes these same writes for structural headers. Share that index; previews build it once for their evaluated batch.
        let preview;
        let indexed = if let Some(structural) = self.structural {
            structural.writes()
        } else {
            preview = PreparedLookup::new(origins, control)?;
            &preview
        };
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
        for (field, &dimensions) in &fields {
            let field = field.bytes();
            self.with_source(field, dimensions, self.after, &mut |current| {
                visit_field_origins(self.layout, indexed, field, control, &mut |change| {
                    if current.document_origin(change.document, control)? != Some(change.origin) {
                        return Err(invalid(
                            "population input differs from complete canonical replacement",
                        ));
                    }
                    Ok(())
                })
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
                self.replace(&output, key, template, &header, indexed)
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
        origins: &PreparedLookup<'_>,
    ) -> VersionResult<()> {
        let control = self.control;
        let mut state = header.state;
        self.with_source(
            &header.field,
            state.dimensions(),
            self.before,
            &mut |previous| {
                visit_field_origins(
                    self.layout,
                    origins,
                    &header.field,
                    control,
                    &mut |change| {
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
                        Ok(())
                    },
                )
            },
        )?;
        let bytes = self.layout.encode_header(key, template, state, control)?;
        self.put(output, key, Some(&bytes))
    }

    fn visit_origins(
        &self,
        origins: &PreparedRecordCommit,
        visit: &mut dyn FnMut(&DiskANNPopulationOrigin) -> VersionResult<()>,
    ) -> VersionResult<()> {
        let mut writes = origins.writes();
        while let Some(write) =
            writes.next_with_kind(RecordWriteKind::DiskANNOrigin, self.control)?
        {
            if let Some(value) = write.value() {
                let origin = self.layout.origin(write.key(), value, self.control)?;
                visit(&origin)?;
            }
        }
        self.control.check()?;
        Ok(())
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

fn visit_field_origins(
    layout: &dyn DiskANNPopulationRecordLayout,
    origins: &PreparedLookup<'_>,
    field: &[u8],
    control: &StorageReadControl,
    visit: &mut dyn FnMut(&DiskANNPopulationOrigin) -> VersionResult<()>,
) -> VersionResult<()> {
    let prefix = layout.origin_prefix(field, control)?;
    origins.visit_prefix_with_kind(
        &prefix,
        RecordWriteKind::DiskANNOrigin,
        control,
        &mut |write| {
            if let Some(value) = write.value() {
                let origin = layout.origin(write.key(), value, control)?;
                if &*origin.field != field {
                    return Err(invalid("origin range includes another canonical field"));
                }
                visit(&origin)?;
            }
            Ok(true)
        },
    )?;
    control.check()?;
    Ok(())
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
