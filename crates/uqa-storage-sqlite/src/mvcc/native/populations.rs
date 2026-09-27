//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native population envelopes adapt the shared MVCC census and replacement arithmetic.

mod records;
pub(super) mod schema;

use rusqlite::types::ValueRef;
use uqa_core::{memory::BudgetedVec, DocId};
use uqa_storage::{
    diskann_index::{
        format::{DiskANNCanonicalOrigin, DiskANNGeneration},
        DiskANNPopulationState, DiskANNPopulationWitness, DiskANNQueryRead,
    },
    mvcc::{
        DatabaseId, DiskANNPopulationHeader, DiskANNPopulationOrigin,
        DiskANNPopulationRecordLayout, MergedRecordSnapshot, VersionResult,
    },
    read_control::StorageReadControl,
};

use super::{
    decode_record, encode_row, NativeRecord, NativeRecordFamily as Family,
    NativeRecordIdentity as Identity, NativeRecordNamespace, NativeRecordOwner as Owner,
    NativeSnapshot,
};
use records::{bytes, dimensions, generation, generation_bytes, invalid, Address, Field};

pub(crate) fn header_key(
    owner: Owner,
    field: &[u8],
    generation: DiskANNGeneration,
    control: &StorageReadControl,
) -> VersionResult<BudgetedVec<u8>> {
    Identity::new(Family::VectorPopulations, owner)?.encode_key(
        &[
            ValueRef::Text(field),
            ValueRef::Blob(&generation_bytes(generation)),
        ],
        control,
    )
}

pub(crate) fn header_record(
    owner: Owner,
    table: &[u8],
    field: &[u8],
    state: DiskANNPopulationState,
    control: &StorageReadControl,
) -> VersionResult<NativeRecord> {
    NativeRecord::encode(
        Family::VectorPopulations,
        owner,
        &[
            ValueRef::Text(table),
            ValueRef::Text(field),
            ValueRef::Blob(&generation_bytes(state.generation())),
            ValueRef::Blob(&state.encode()),
        ],
        control,
    )
}

fn header<'a>(
    key: &[u8],
    value: &'a [u8],
    control: &StorageReadControl,
) -> VersionResult<(Identity, BudgetedVec<ValueRef<'a>>, DiskANNPopulationState)> {
    let (identity, row) = decode_record(key, value, control)?;
    if identity.family() != Family::VectorPopulations {
        return Err(invalid());
    }
    let payload = bytes(row[3])?;
    let state = DiskANNPopulationState::decode(
        payload,
        generation(bytes(row[2])?)?,
        dimensions(payload, 48)?,
    )?;
    Ok((identity, row, state))
}

impl DiskANNPopulationRecordLayout for NativeRecordNamespace {
    fn origin(
        &self,
        key: &[u8],
        value: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<DiskANNPopulationOrigin> {
        let (identity, row) = decode_record(key, value, control)?;
        if identity.family() != Family::VectorOrigins {
            return Err(invalid());
        }
        let payload = bytes(row[3])?;
        Ok(DiskANNPopulationOrigin {
            field: Field::encode(identity.owner(), row[0], row[1], control)?,
            document: u64::try_from(row[2].as_i64().map_err(|_| invalid())?)
                .map_err(|_| invalid())?,
            origin: DiskANNCanonicalOrigin::decode(payload, dimensions(payload, 40)?)?,
        })
    }

    fn origin_header_prefix(
        &self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>> {
        records::origin_header_prefix(key, control)
    }

    fn header_prefix(
        &self,
        field: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>> {
        let field = Field::decode(field, control)?;
        Identity::new(Family::VectorPopulations, field.owner)?
            .encode_prefix(&[ValueRef::Text(&field.name)], control)
    }

    fn header(
        &self,
        key: &[u8],
        value: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<DiskANNPopulationHeader> {
        let (identity, row, state) = header(key, value, control)?;
        Ok(DiskANNPopulationHeader {
            field: Field::encode(identity.owner(), row[0], row[1], control)?,
            state,
        })
    }

    fn witness_prefix(
        &self,
        header: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>> {
        let address = Address::decode(header, control)?;
        if address.document.is_some() {
            return Err(invalid());
        }
        address.prefix(Family::VectorPopulationWitnesses, control)
    }

    fn preview_header(
        &self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>> {
        Address::decode(key, control)?.prefix(Family::VectorPopulations, control)
    }

    fn witness_key(
        &self,
        header: &[u8],
        document: DocId,
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>> {
        let address = Address::decode(header, control)?;
        if address.document.is_some() {
            return Err(invalid());
        }
        Identity::new(Family::VectorPopulationWitnesses, address.owner)?.encode_key(
            &[
                ValueRef::Text(&address.field),
                ValueRef::Blob(&generation_bytes(address.generation)),
                ValueRef::Integer(i64::try_from(document).map_err(|_| invalid())?),
            ],
            control,
        )
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
        let (identity, row) = decode_record(key, value, control)?;
        if identity.family() != Family::VectorPopulationWitnesses
            || generation(bytes(row[2])?)? != state.generation()
            || row[3]
                .as_i64()
                .ok()
                .and_then(|value| u64::try_from(value).ok())
                != Some(document)
            || origin.dimensions() != state.dimensions()
        {
            return Err(invalid());
        }
        Ok(DiskANNPopulationWitness::decode(
            bytes(row[4])?,
            state.generation(),
            document,
            origin,
        )?)
    }

    fn encode_header(
        &self,
        key: &[u8],
        template: &[u8],
        state: DiskANNPopulationState,
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>> {
        let (_, row, original) = header(key, template, control)?;
        if state.generation() != original.generation()
            || state.dimensions() != original.dimensions()
        {
            return Err(invalid());
        }
        encode_row(
            &[row[0], row[1], row[2], ValueRef::Blob(&state.encode())],
            control,
        )
    }

    fn encode_witness(
        &self,
        key: &[u8],
        template: &[u8],
        witness: DiskANNPopulationWitness,
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>> {
        let header_key = self.preview_header(key, control)?;
        let (_, row, state) = header(&header_key, template, control)?;
        let document = i64::try_from(witness.document()).map_err(|_| invalid())?;
        let result = encode_row(
            &[
                row[0],
                row[1],
                row[2],
                ValueRef::Integer(document),
                ValueRef::Blob(&witness.encode()),
            ],
            control,
        )?;
        self.witness(
            key,
            &result,
            state,
            witness.document(),
            witness.origin(),
            control,
        )?;
        Ok(result)
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
        let field = Field::decode(field, control)?;
        let snapshot = NativeSnapshot {
            view: view.try_clone()?,
            control: control.clone(),
            database: self.0,
            history,
        };
        let source = crate::RetainedSQLiteDiskANNCanonical::from_population(
            snapshot,
            field.owner,
            field.table,
            field.name,
            dimensions,
        )?;
        visit(&source)
    }
}
