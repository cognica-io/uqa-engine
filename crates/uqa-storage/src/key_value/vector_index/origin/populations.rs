//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Key/Value population addresses share the original canonical field and generation identities.

mod keys;
pub(in crate::key_value) use keys::{field_prefixes, table_prefixes, ROOT};

use crate::diskann_index::{
    format::{DiskANNCanonicalOrigin, DiskANNGeneration},
    DiskANNPopulationState, DiskANNPopulationWitness, DiskANNQueryRead,
};
use crate::key_value::KeyValueRead;
use crate::mvcc::{
    DatabaseId, DiskANNPopulationHeader, DiskANNPopulationOrigin, DiskANNPopulationRecordLayout,
    MergedRecordSnapshot, RecordRead, VersionResult,
};
use crate::read_control::StorageReadControl;
use keys::{concatenate, dimensions, field_len, invalid, parts, HEADER, WITNESS};
use uqa_core::{memory::BudgetedVec, DocId};

/// Physical population mapping for a versioned Key/Value provider that has enabled the corresponding writer format.
pub struct KeyValueDiskANNPopulationRecords;

impl KeyValueDiskANNPopulationRecords {
    /// Address a generation's counts under an already encoded complete canonical vector-field prefix. Publication still requires the catalog/head owner's authority and an enabled provider writer format.
    pub fn header_key(
        field: &[u8],
        generation: DiskANNGeneration,
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>> {
        keys::header(field, generation, control)
    }
}

impl DiskANNPopulationRecordLayout for KeyValueDiskANNPopulationRecords {
    fn origin(
        &self,
        key: &[u8],
        value: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<DiskANNPopulationOrigin> {
        control.check()?;
        let raw = key
            .strip_prefix(super::ROOT)
            .ok_or(invalid("population input is not a canonical origin"))?;
        let end = field_len(raw)?;
        if raw.len() != end + 8 {
            return Err(invalid("invalid canonical origin key width"));
        }
        let origin = DiskANNCanonicalOrigin::decode(value, dimensions(value, 40)?)?;
        Ok(DiskANNPopulationOrigin {
            field: concatenate(&[&raw[..end]], control)?,
            document: u64::from_be_bytes(raw[end..].try_into().expect("validated width")),
            origin,
        })
    }

    fn header_prefix(
        &self,
        field: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>> {
        if field_len(field)? != field.len() {
            return Err(invalid("population field has trailing bytes"));
        }
        concatenate(&[ROOT, &[HEADER], field], control)
    }

    fn header(
        &self,
        key: &[u8],
        value: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<DiskANNPopulationHeader> {
        let (field, generation) = parts(key, HEADER)?;
        Ok(DiskANNPopulationHeader {
            field: concatenate(&[field], control)?,
            state: DiskANNPopulationState::decode(value, generation, dimensions(value, 48)?)?,
        })
    }

    fn witness_prefix(
        &self,
        header: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>> {
        parts(header, HEADER)?;
        let mut key = concatenate(&[header], control)?;
        key[ROOT.len()] = WITNESS;
        Ok(key)
    }

    fn preview_header(
        &self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>> {
        let kind = *key
            .get(ROOT.len())
            .ok_or(invalid("truncated population key"))?;
        if !matches!(kind, HEADER | WITNESS) {
            return Err(invalid("invalid population preview kind"));
        }
        let (field, generation) = parts(key, kind)?;
        keys::header(field, generation, control)
    }

    fn witness_key(
        &self,
        header: &[u8],
        document: DocId,
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>> {
        let mut key = self.witness_prefix(header, control)?;
        key.extend_from_slice(&document.to_be_bytes())?;
        Ok(key)
    }

    fn witness(
        &self,
        key: &[u8],
        value: &[u8],
        state: DiskANNPopulationState,
        document: DocId,
        origin: DiskANNCanonicalOrigin,
        control: &StorageReadControl,
    ) -> VersionResult<DiskANNPopulationWitness> {
        control.check()?;
        let (_, generation) = parts(key, WITNESS)?;
        if generation != state.generation() || key[key.len() - 8..] != document.to_be_bytes() {
            return Err(invalid(
                "population witness key differs from selected document",
            ));
        }
        Ok(DiskANNPopulationWitness::decode(
            value, generation, document, origin,
        )?)
    }

    fn encode_header(
        &self,
        key: &[u8],
        template: &[u8],
        state: DiskANNPopulationState,
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>> {
        let header = self.header(key, template, control)?;
        if state.generation() != header.state.generation()
            || state.dimensions() != header.state.dimensions()
        {
            return Err(invalid(
                "replacement population header changed its identity",
            ));
        }
        concatenate(&[&state.encode()], control)
    }

    fn encode_witness(
        &self,
        key: &[u8],
        template: &[u8],
        witness: DiskANNPopulationWitness,
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>> {
        let header_key = self.preview_header(key, control)?;
        let header = self.header(&header_key, template, control)?;
        if witness.origin().dimensions() != header.state.dimensions() {
            return Err(invalid("population witness dimensions changed"));
        }
        let bytes = witness.encode();
        self.witness(
            key,
            &bytes,
            header.state,
            witness.document(),
            witness.origin(),
            control,
        )?;
        concatenate(&[&bytes], control)
    }

    fn with_canonical(
        &self,
        field: &[u8],
        dimensions: u32,
        view: &MergedRecordSnapshot,
        history: DatabaseId,
        control: &StorageReadControl,
        visit: &mut dyn FnMut(&dyn DiskANNQueryRead) -> VersionResult<()>,
    ) -> VersionResult<()> {
        if field_len(field)? != field.len() {
            return Err(invalid("canonical population field has trailing bytes"));
        }
        let origins = concatenate(&[super::ROOT, field], control)?;
        let changes = concatenate(&[super::journal::ROOT, field], control)?;
        let read = RecordRead::new(view, history, control).retain(&[field, &origins, &changes])?;
        let source = super::RetainedDiskANNCanonical::new(
            read, field, &origins, &changes, dimensions, control,
        )?;
        visit(&source)
    }
}
