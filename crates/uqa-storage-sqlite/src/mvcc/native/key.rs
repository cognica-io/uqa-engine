//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered native record identities separate physical families, object incarnations and primary-key components.

use rusqlite::types::ValueRef;
use uqa_core::memory::{BudgetedVec, MemoryError};
use uqa_storage::mvcc::{DatabaseId, VersionResult};
use uqa_storage::read_control::StorageReadControl;

use super::{invalid, NativeRecordFamily};

const PREFIX: &[u8] = b"\0uqa-native-record\x01";

/// The database incarnation owns global catalog/name records. Tuple and index records require the object's identity and storage generation, independently of its current SQL name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeRecordOwner {
    Database(DatabaseId),
    Object {
        identity: [u8; 16],
        generation: [u8; 16],
    },
}

impl NativeRecordOwner {
    /// Whether this owner's rows are the documents of `namespace`. An object's identity and storage generation key its rows and name the namespace of its document identities alike, so what the namespace's watermark shows about an identity holds for the rows of this owner and of no other.
    pub(crate) fn stores(
        self,
        namespace: uqa_storage::document_store::identifiers::DocumentIdNamespace,
    ) -> bool {
        self == Self::Object {
            identity: namespace.object,
            generation: namespace.generation,
        }
    }
}

/// A provider-assigned physical family and stable owner. The durable family number must come from a fixed format registry, never schema enumeration order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeRecordIdentity {
    family: NativeRecordFamily,
    owner: NativeRecordOwner,
}

impl NativeRecordIdentity {
    /// Address every retained generation of one object without scanning other object payloads.
    pub(crate) fn object_prefix(
        family: NativeRecordFamily,
        identity: [u8; 16],
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>> {
        if !family.layout().object_owned || identity == [0; 16] {
            return Err(invalid(
                "native object prefix requires an assigned object identity",
            ));
        }
        let mut key = Self::family_prefix(family, control)?;
        key.push(1)?;
        key.extend_from_slice(&identity)?;
        Ok(key)
    }

    pub(crate) fn family_prefix(
        family: NativeRecordFamily,
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>> {
        control.cancellation().check()?;
        let mut key = BudgetedVec::new(control.memory());
        key.extend_from_slice(PREFIX)?;
        key.extend_from_slice(&family.id().to_be_bytes())?;
        Ok(key)
    }

    pub fn new(family: NativeRecordFamily, owner: NativeRecordOwner) -> VersionResult<Self> {
        if family.layout().object_owned != matches!(owner, NativeRecordOwner::Object { .. }) {
            return Err(invalid("native family does not match its owner kind"));
        }
        if let NativeRecordOwner::Object {
            identity,
            generation,
        } = owner
        {
            if identity == [0; 16] || generation == [0; 16] {
                return Err(invalid(
                    "native object identity and generation must be assigned",
                ));
            }
        }
        Ok(Self { family, owner })
    }

    pub fn family(self) -> NativeRecordFamily {
        self.family
    }

    pub fn owner(self) -> NativeRecordOwner {
        self.owner
    }

    /// Compare the canonical encoding directly, without allocating a second copy of a retained record key.
    pub(super) fn matches_row_key(
        self,
        key: &[u8],
        values: &[ValueRef<'_>],
        control: &StorageReadControl,
    ) -> VersionResult<bool> {
        let owner_bytes = match self.owner {
            NativeRecordOwner::Database(_) => 16,
            NativeRecordOwner::Object { .. } => 32,
        };
        // decode_record already validated this exact family/owner header.
        let mut input = &key[PREFIX.len() + 2 + 1 + owner_bytes..];
        for &column in self.family.layout().identity_columns {
            control.cancellation().check()?;
            let matches = match values[column] {
                ValueRef::Integer(value) => {
                    let ordered = u64::from_be_bytes(value.to_be_bytes()) ^ (1 << 63);
                    consume(&mut input, &[1]) && consume(&mut input, &ordered.to_be_bytes())
                }
                ValueRef::Text(bytes) => {
                    consume(&mut input, &[2]) && matches_escaped(&mut input, bytes, control)?
                }
                ValueRef::Blob(bytes) => {
                    consume(&mut input, &[3]) && matches_escaped(&mut input, bytes, control)?
                }
                ValueRef::Null | ValueRef::Real(_) => {
                    return Err(invalid("native primary key must be INTEGER, TEXT or BLOB"));
                }
            };
            if !matches {
                return Ok(false);
            }
        }
        control.cancellation().check()?;
        Ok(input.is_empty())
    }

    pub(super) fn validate_row(self, values: &[ValueRef<'_>]) -> VersionResult<()> {
        let layout = self.family.layout();
        layout.validate_values(values)?;
        if self.family.is_standalone_graph() {
            super::standalone_graph::validate_values(self.family, values)?;
        }
        let generation_column = match self.family {
            NativeRecordFamily::Tables => "storage_generation",
            NativeRecordFamily::Sequences | NativeRecordFamily::SequenceValues => {
                "definition_generation"
            }
            _ => return Ok(()),
        };
        let NativeRecordOwner::Object {
            identity,
            generation,
        } = self.owner
        else {
            return Err(invalid("native definition requires an object owner"));
        };
        for (name, expected) in [("object_id", identity), (generation_column, generation)] {
            let column = layout
                .columns
                .iter()
                .position(|column| *column == name)
                .ok_or_else(|| invalid("native definition layout lacks its identity column"))?;
            if values[column] != ValueRef::Blob(&expected) {
                return Err(invalid(
                    "native definition does not match its owner generation",
                ));
            }
        }
        Ok(())
    }

    pub(crate) fn decode(key: &[u8]) -> VersionResult<Self> {
        let bytes = key
            .strip_prefix(PREFIX)
            .ok_or_else(|| invalid("unknown native record key codec"))?;
        let family_bytes = bytes
            .get(..2)
            .ok_or_else(|| invalid("truncated native record family"))?;
        let family =
            NativeRecordFamily::from_id(u16::from_be_bytes([family_bytes[0], family_bytes[1]]))
                .ok_or_else(|| invalid("unknown native record family"))?;
        let array = |start: usize| -> VersionResult<[u8; 16]> {
            bytes
                .get(start..start + 16)
                .and_then(|slice| slice.try_into().ok())
                .ok_or_else(|| invalid("truncated native record owner"))
        };
        let owner = match bytes.get(2) {
            Some(0) => NativeRecordOwner::Database(DatabaseId::from_bytes(array(3)?)),
            Some(1) => NativeRecordOwner::Object {
                identity: array(3)?,
                generation: array(19)?,
            },
            _ => return Err(invalid("unknown native record owner kind")),
        };
        Self::new(family, owner)
    }

    /// Encode a complete primary key. Integer order is signed `SQLite` order; TEXT and BLOB order is binary byte order. NULL/REAL primary keys require a different equality contract and are rejected.
    pub fn encode_key(
        self,
        components: &[ValueRef<'_>],
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>> {
        if components.len() != self.family.layout().identity_columns.len() {
            return Err(invalid("native primary key does not match its layout"));
        }
        self.encode_prefix(components, control)
    }

    /// Encode an owner/component prefix for a bounded scan over this family and generation.
    pub fn encode_prefix(
        self,
        components: &[ValueRef<'_>],
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>> {
        control.cancellation().check()?;
        if components.len() > self.family.layout().identity_columns.len() {
            return Err(invalid("native primary key prefix exceeds its layout"));
        }
        let mut key = Self::family_prefix(self.family, control)?;
        match self.owner {
            NativeRecordOwner::Database(database) => {
                key.push(0)?;
                key.extend_from_slice(&database.as_bytes())?;
            }
            NativeRecordOwner::Object {
                identity,
                generation,
            } => {
                key.push(1)?;
                key.extend_from_slice(&identity)?;
                key.extend_from_slice(&generation)?;
            }
        }
        for (component, &column) in components.iter().zip(self.family.layout().identity_columns) {
            control.cancellation().check()?;
            let expected = self.family.layout().column_types[column];
            if !expected.accepts(*component) {
                return Err(invalid(
                    "native primary key storage class does not match its layout",
                ));
            }
            match component {
                ValueRef::Integer(value) => {
                    key.push(1)?;
                    let ordered = u64::from_be_bytes(value.to_be_bytes()) ^ (1 << 63);
                    key.extend_from_slice(&ordered.to_be_bytes())?;
                }
                ValueRef::Text(bytes) => {
                    std::str::from_utf8(bytes)
                        .map_err(|_| invalid("native text key is not UTF-8"))?;
                    key.push(2)?;
                    escaped_bytes(&mut key, bytes, control)?;
                }
                ValueRef::Blob(bytes) => {
                    key.push(3)?;
                    escaped_bytes(&mut key, bytes, control)?;
                }
                ValueRef::Null | ValueRef::Real(_) => {
                    return Err(invalid("native primary key must be INTEGER, TEXT or BLOB"));
                }
            }
        }
        Ok(key)
    }

    /// Encode a byte prefix within a single BLOB key, without its component terminator. Complete keys retain the existing escaped byte order.
    pub(crate) fn encode_blob_prefix(
        self,
        bytes: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>> {
        self.encode_variable_prefix(bytes, super::NativeColumnType::Blob, 3, control)
    }

    /// Encode a UTF-8 prefix within one TEXT identity, without a component terminator.
    pub(crate) fn encode_text_prefix(
        self,
        bytes: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>> {
        std::str::from_utf8(bytes).map_err(|_| invalid("native text prefix is not UTF-8"))?;
        self.encode_variable_prefix(bytes, super::NativeColumnType::Text, 2, control)
    }

    fn encode_variable_prefix(
        self,
        bytes: &[u8],
        kind: super::NativeColumnType,
        tag: u8,
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>> {
        let layout = self.family.layout();
        if layout.identity_columns.len() != 1
            || layout.column_types[layout.identity_columns[0]] != kind
        {
            return Err(invalid(
                "native variable prefix requires one matching identity column",
            ));
        }
        let mut key = self.encode_prefix(&[], control)?;
        key.push(tag)?;
        escaped_prefix(&mut key, bytes, control)?;
        Ok(key)
    }

    /// Validate every ordered component, including keys for an absent tombstone.
    pub(super) fn decode_full(key: &[u8], control: &StorageReadControl) -> VersionResult<Self> {
        Self::visit_key_components(key, control, |_, _| Ok(()))
    }

    /// Decode ordered keys without loading row payloads. Borrowed component buffers are valid only during the callback.
    pub(crate) fn visit_key_components(
        key: &[u8],
        control: &StorageReadControl,
        visit: impl FnMut(usize, ValueRef<'_>) -> VersionResult<()>,
    ) -> VersionResult<Self> {
        let count = Self::decode(key)?.family.layout().identity_columns.len();
        Self::visit_prefix_components(key, count, control, visit)
    }

    /// Validate an exact number of complete primary-key components in an encoded prefix.
    pub(crate) fn visit_prefix_components(
        key: &[u8],
        count: usize,
        control: &StorageReadControl,
        mut visit: impl FnMut(usize, ValueRef<'_>) -> VersionResult<()>,
    ) -> VersionResult<Self> {
        let identity = Self::decode(key)?;
        if count > identity.family.layout().identity_columns.len() {
            return Err(invalid("native prefix has too many components"));
        }
        let header = identity.encode_prefix(&[], control)?;
        let mut input = &key[header.len()..];
        for (component, &column) in identity
            .family
            .layout()
            .identity_columns
            .iter()
            .take(count)
            .enumerate()
        {
            control.cancellation().check()?;
            let (&tag, rest) = input
                .split_first()
                .ok_or_else(|| invalid("truncated native key component"))?;
            input = rest;
            let mut bytes = BudgetedVec::new(control.memory());
            let value = match tag {
                1 => {
                    let (integer, rest) = input
                        .split_at_checked(8)
                        .ok_or_else(|| invalid("truncated native integer key"))?;
                    input = rest;
                    let ordered = u64::from_be_bytes(integer.try_into().expect("eight bytes"));
                    ValueRef::Integer(i64::from_be_bytes((ordered ^ (1 << 63)).to_be_bytes()))
                }
                2 | 3 => {
                    loop {
                        control.cancellation().check()?;
                        let (&byte, rest) = input
                            .split_first()
                            .ok_or_else(|| invalid("unterminated native binary key"))?;
                        input = rest;
                        if byte == 0 {
                            let (&escape, rest) = input
                                .split_first()
                                .ok_or_else(|| invalid("truncated native key escape"))?;
                            input = rest;
                            match escape {
                                0 => break,
                                255 => {}
                                _ => return Err(invalid("invalid native key escape")),
                            }
                        }
                        bytes.push(byte)?;
                    }
                    if tag == 2 {
                        std::str::from_utf8(&bytes)
                            .map_err(|_| invalid("native text key is not UTF-8"))?;
                        ValueRef::Text(&bytes)
                    } else {
                        ValueRef::Blob(&bytes)
                    }
                }
                _ => return Err(invalid("unknown native key component")),
            };
            if !identity.family.layout().column_types[column].accepts(value) {
                return Err(invalid("native key component does not match its layout"));
            }
            visit(component, value)?;
        }
        if !input.is_empty() {
            return Err(invalid("native key has trailing components"));
        }
        Ok(identity)
    }
}

fn consume(input: &mut &[u8], expected: &[u8]) -> bool {
    if let Some(rest) = input.strip_prefix(expected) {
        *input = rest;
        true
    } else {
        false
    }
}

fn matches_escaped(
    input: &mut &[u8],
    bytes: &[u8],
    control: &StorageReadControl,
) -> VersionResult<bool> {
    for (index, &byte) in bytes.iter().enumerate() {
        if index % 1024 == 0 {
            control.cancellation().check()?;
        }
        if !consume(input, &[byte]) || (byte == 0 && !consume(input, &[255])) {
            return Ok(false);
        }
    }
    Ok(consume(input, &[0, 0]))
}

fn escaped_bytes(
    output: &mut BudgetedVec<u8>,
    bytes: &[u8],
    control: &StorageReadControl,
) -> VersionResult<()> {
    escaped_prefix(output, bytes, control)?;
    output.extend_from_slice(&[0, 0])?;
    Ok(())
}

fn escaped_prefix(
    output: &mut BudgetedVec<u8>,
    bytes: &[u8],
    control: &StorageReadControl,
) -> VersionResult<()> {
    output.reserve(
        bytes
            .len()
            .checked_add(2)
            .ok_or(MemoryError::SizeOverflow)?,
    )?;
    for chunk in bytes.chunks(1024) {
        control.cancellation().check()?;
        for &byte in chunk {
            output.push(byte)?;
            if byte == 0 {
                output.push(255)?;
            }
        }
    }
    Ok(())
}
