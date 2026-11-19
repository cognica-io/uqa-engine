//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed graph encoding preserves the existing ordered primary image.

use super::super::native::{NativeMapIter, NativeSetIter};
use super::{
    invalid, validation::Validation, LabelKind, MemoryGraphStore, NativeLabels, Partition,
    StorageBackendResult, StorageReadControl, MAGIC,
};
use std::{
    collections::{btree_map, BTreeMap},
    iter::Map,
};
use uqa_core::{value_image::ValueImageReader, Edge, Value, Vertex};
use uqa_storage::image_io::{ImagePreparation, ImageRead};

type Fields<'a> =
    Map<btree_map::Iter<'a, String, Value>, fn((&'a String, &'a Value)) -> (&'a str, &'a Value)>;

fn field<'a>((name, value): (&'a String, &'a Value)) -> (&'a str, &'a Value) {
    (name.as_str(), value)
}

pub(super) enum Piece<'a> {
    Borrowed(&'a [u8]),
    Fixed { bytes: [u8; 32], length: usize },
}

impl Piece<'_> {
    fn fixed(input: &[u8]) -> Self {
        let mut bytes = [0; 32];
        bytes[..input.len()].copy_from_slice(input);
        Self::Fixed {
            bytes,
            length: input.len(),
        }
    }
    fn count(value: usize) -> StorageBackendResult<Self> {
        Ok(Self::fixed(
            &u64::try_from(value)
                .map_err(|_| uqa_core::memory::MemoryError::SizeOverflow)?
                .to_le_bytes(),
        ))
    }
    pub(super) fn bytes(&self) -> &[u8] {
        match self {
            Self::Borrowed(bytes) => bytes,
            Self::Fixed { bytes, length } => &bytes[..*length],
        }
    }
}

#[derive(Clone, Copy)]
enum Stage {
    VertexCount,
    Vertices,
    EdgeCount,
    Edges,
    EntityLabel { edge: bool },
    EntityFields { edge: bool },
    GraphCount,
    Graphs,
    GraphName,
    VertexMemberCount,
    VertexMembers,
    EdgeMemberCount,
    EdgeMembers,
    RegistryCount,
    Registries,
    RegistryName,
    Allocation,
    LabelCount,
    Labels,
    LabelName(u32),
    LabelId(u32),
    KindCount,
    Kinds,
    KindName(LabelKind),
    Kind(LabelKind),
    SequenceCount,
    Sequences,
    DroppedCount,
    Dropped,
    Complete,
}

pub(super) struct Encoder<'a> {
    store: &'a MemoryGraphStore,
    control: &'a StorageReadControl,
    stage: Stage,
    vertices: NativeMapIter<'a, u64, Vertex>,
    edges: NativeMapIter<'a, u64, Edge>,
    graphs: NativeMapIter<'a, String, Partition>,
    registries: NativeMapIter<'a, String, NativeLabels>,
    name: &'a str,
    properties: Option<&'a BTreeMap<String, Value>>,
    values: Option<ValueImageReader<'a, 'a, Fields<'a>>>,
    partition: Option<&'a Partition>,
    ids: Option<NativeSetIter<'a, u64>>,
    registry: Option<&'a NativeLabels>,
    labels: Option<NativeMapIter<'a, String, u32>>,
    kinds: Option<NativeMapIter<'a, String, LabelKind>>,
    sequences: Option<NativeMapIter<'a, u32, u64>>,
    dropped: Option<NativeSetIter<'a, u32>>,
    pending: Option<Piece<'a>>,
    position: usize,
}

impl<'a> Encoder<'a> {
    pub(super) fn new(store: &'a MemoryGraphStore, control: &'a StorageReadControl) -> Self {
        let mut header = [0; 24];
        header[..8].copy_from_slice(MAGIC);
        header[8..16].copy_from_slice(&store.next_vertex_id.to_le_bytes());
        header[16..].copy_from_slice(&store.next_edge_id.to_le_bytes());
        Self {
            store,
            control,
            stage: Stage::VertexCount,
            vertices: store.vertices.iter(),
            edges: store.edges.iter(),
            graphs: store.graphs.iter(),
            registries: store.label_registries.iter(),
            name: "",
            properties: None,
            values: None,
            partition: None,
            ids: None,
            registry: None,
            labels: None,
            kinds: None,
            sequences: None,
            dropped: None,
            pending: Some(Piece::fixed(&header)),
            position: 0,
        }
    }

    fn next_piece(&mut self) -> StorageBackendResult<Option<Piece<'a>>> {
        loop {
            self.control.check()?;
            let piece = match self.stage {
                Stage::VertexCount
                | Stage::Vertices
                | Stage::EdgeCount
                | Stage::Edges
                | Stage::EntityLabel { .. }
                | Stage::EntityFields { .. } => self.entity_piece()?,
                Stage::GraphCount
                | Stage::Graphs
                | Stage::GraphName
                | Stage::VertexMemberCount
                | Stage::VertexMembers
                | Stage::EdgeMemberCount
                | Stage::EdgeMembers => self.graph_piece()?,
                Stage::RegistryCount
                | Stage::Registries
                | Stage::RegistryName
                | Stage::Allocation
                | Stage::LabelCount
                | Stage::Labels
                | Stage::LabelName(_)
                | Stage::LabelId(_) => self.registry_header_piece()?,
                Stage::KindCount
                | Stage::Kinds
                | Stage::KindName(_)
                | Stage::Kind(_)
                | Stage::SequenceCount
                | Stage::Sequences
                | Stage::DroppedCount
                | Stage::Dropped => self.registry_values_piece()?,
                Stage::Complete => return Ok(None),
            };
            if piece.is_some() || self.values.is_some() {
                return Ok(piece);
            }
        }
    }

    fn entity_piece(&mut self) -> StorageBackendResult<Option<Piece<'a>>> {
        let piece = match self.stage {
            Stage::VertexCount => {
                self.stage = Stage::Vertices;
                Piece::count(self.store.vertices.len())?
            }
            Stage::Vertices => {
                let Some((id, vertex)) = self.vertices.next() else {
                    self.stage = Stage::EdgeCount;
                    return Ok(None);
                };
                self.name = &vertex.label;
                self.properties = Some(&vertex.properties);
                self.stage = Stage::EntityLabel { edge: false };
                let mut bytes = [0; 16];
                bytes[..8].copy_from_slice(&id.to_le_bytes());
                bytes[8..].copy_from_slice(Piece::count(vertex.label.len())?.bytes());
                Piece::fixed(&bytes)
            }
            Stage::EdgeCount => {
                self.stage = Stage::Edges;
                Piece::count(self.store.edges.len())?
            }
            Stage::Edges => {
                let Some((id, edge)) = self.edges.next() else {
                    self.stage = Stage::GraphCount;
                    return Ok(None);
                };
                self.name = &edge.label;
                self.properties = Some(&edge.properties);
                self.stage = Stage::EntityLabel { edge: true };
                let mut bytes = [0; 32];
                for (slot, id) in
                    bytes[..24]
                        .chunks_exact_mut(8)
                        .zip([*id, edge.source_id, edge.target_id])
                {
                    slot.copy_from_slice(&id.to_le_bytes());
                }
                bytes[24..].copy_from_slice(Piece::count(edge.label.len())?.bytes());
                Piece::fixed(&bytes)
            }
            Stage::EntityLabel { edge } => {
                self.stage = Stage::EntityFields { edge };
                Piece::Borrowed(self.name.as_bytes())
            }
            Stage::EntityFields { edge } => {
                let fields: Fields<'a> = self
                    .properties
                    .take()
                    .ok_or_else(invalid)?
                    .iter()
                    .map(field);
                self.values = Some(ValueImageReader::from_fields(
                    fields,
                    self.control.memory(),
                    self.control.cancellation(),
                ));
                self.stage = if edge { Stage::Edges } else { Stage::Vertices };
                return Ok(None);
            }
            _ => unreachable!("encoding continuation belongs to another section"),
        };
        Ok(Some(piece))
    }

    fn graph_piece(&mut self) -> StorageBackendResult<Option<Piece<'a>>> {
        let piece = match self.stage {
            Stage::GraphCount => {
                self.stage = Stage::Graphs;
                Piece::count(self.store.graphs.len())?
            }
            Stage::Graphs => {
                let Some((name, part)) = self.graphs.next() else {
                    self.stage = Stage::RegistryCount;
                    return Ok(None);
                };
                self.name = name;
                self.partition = Some(part);
                self.stage = Stage::GraphName;
                Piece::count(name.len())?
            }
            Stage::GraphName => {
                self.stage = Stage::VertexMemberCount;
                Piece::Borrowed(self.name.as_bytes())
            }
            Stage::VertexMemberCount => {
                let ids = &self.partition.ok_or_else(invalid)?.vertex_ids;
                self.ids = Some(ids.iter());
                self.stage = Stage::VertexMembers;
                Piece::count(ids.len())?
            }
            Stage::VertexMembers | Stage::EdgeMembers => {
                if let Some(id) = self.ids.as_mut().and_then(Iterator::next) {
                    Piece::fixed(&id.to_le_bytes())
                } else {
                    self.ids = None;
                    self.stage = if matches!(self.stage, Stage::VertexMembers) {
                        Stage::EdgeMemberCount
                    } else {
                        Stage::Graphs
                    };
                    return Ok(None);
                }
            }
            Stage::EdgeMemberCount => {
                let ids = &self.partition.take().ok_or_else(invalid)?.edge_ids;
                self.ids = Some(ids.iter());
                self.stage = Stage::EdgeMembers;
                Piece::count(ids.len())?
            }
            _ => unreachable!("encoding continuation belongs to another section"),
        };
        Ok(Some(piece))
    }

    fn registry_header_piece(&mut self) -> StorageBackendResult<Option<Piece<'a>>> {
        let piece = match self.stage {
            Stage::RegistryCount => {
                self.stage = Stage::Registries;
                Piece::count(self.store.label_registries.len())?
            }
            Stage::Registries => {
                let Some((name, registry)) = self.registries.next() else {
                    self.stage = Stage::Complete;
                    return Ok(None);
                };
                self.name = name;
                self.registry = Some(registry);
                self.stage = Stage::RegistryName;
                Piece::count(name.len())?
            }
            Stage::RegistryName => {
                self.stage = Stage::Allocation;
                Piece::Borrowed(self.name.as_bytes())
            }
            Stage::Allocation => {
                let registry = self.registry.ok_or_else(invalid)?;
                let mut bytes = [0; 20];
                bytes[..16].copy_from_slice(&registry.allocation_id);
                bytes[16..].copy_from_slice(&registry.next_label_id.to_le_bytes());
                self.stage = Stage::LabelCount;
                Piece::fixed(&bytes)
            }
            Stage::LabelCount => {
                let registry = self.registry.ok_or_else(invalid)?;
                self.labels = Some(registry.labels.iter());
                self.stage = Stage::Labels;
                Piece::count(registry.labels.len())?
            }
            Stage::Labels => {
                let Some((name, id)) = self.labels.as_mut().and_then(Iterator::next) else {
                    self.labels = None;
                    self.stage = Stage::KindCount;
                    return Ok(None);
                };
                self.name = name;
                self.stage = Stage::LabelName(*id);
                Piece::count(name.len())?
            }
            Stage::LabelName(id) => {
                self.stage = Stage::LabelId(id);
                Piece::Borrowed(self.name.as_bytes())
            }
            Stage::LabelId(id) => {
                self.stage = Stage::Labels;
                Piece::fixed(&id.to_le_bytes())
            }
            _ => unreachable!("encoding continuation belongs to another section"),
        };
        Ok(Some(piece))
    }

    fn registry_values_piece(&mut self) -> StorageBackendResult<Option<Piece<'a>>> {
        let piece = match self.stage {
            Stage::KindCount => {
                let registry = self.registry.ok_or_else(invalid)?;
                self.kinds = Some(registry.kinds.iter());
                self.stage = Stage::Kinds;
                Piece::count(registry.kinds.len())?
            }
            Stage::Kinds => {
                let Some((name, kind)) = self.kinds.as_mut().and_then(Iterator::next) else {
                    self.kinds = None;
                    self.stage = Stage::SequenceCount;
                    return Ok(None);
                };
                self.name = name;
                self.stage = Stage::KindName(*kind);
                Piece::count(name.len())?
            }
            Stage::KindName(kind) => {
                self.stage = Stage::Kind(kind);
                Piece::Borrowed(self.name.as_bytes())
            }
            Stage::Kind(kind) => {
                self.stage = Stage::Kinds;
                Piece::fixed(&[match kind {
                    LabelKind::Vertex => 0,
                    LabelKind::Edge => 1,
                }])
            }
            Stage::SequenceCount => {
                let registry = self.registry.ok_or_else(invalid)?;
                self.sequences = Some(registry.sequences.iter());
                self.stage = Stage::Sequences;
                Piece::count(registry.sequences.len())?
            }
            Stage::Sequences => {
                let Some((id, sequence)) = self.sequences.as_mut().and_then(Iterator::next) else {
                    self.sequences = None;
                    self.stage = Stage::DroppedCount;
                    return Ok(None);
                };
                let mut bytes = [0; 12];
                bytes[..4].copy_from_slice(&id.to_le_bytes());
                bytes[4..].copy_from_slice(&sequence.to_le_bytes());
                Piece::fixed(&bytes)
            }
            Stage::DroppedCount => {
                let registry = self.registry.take().ok_or_else(invalid)?;
                self.dropped = Some(registry.dropped_label_ids.iter());
                self.stage = Stage::Dropped;
                Piece::count(registry.dropped_label_ids.len())?
            }
            Stage::Dropped => {
                let Some(id) = self.dropped.as_mut().and_then(Iterator::next) else {
                    self.dropped = None;
                    self.stage = Stage::Registries;
                    return Ok(None);
                };
                Piece::fixed(&id.to_le_bytes())
            }
            _ => unreachable!("encoding continuation belongs to another section"),
        };
        Ok(Some(piece))
    }

    pub(super) fn read(&mut self, output: &mut [u8]) -> StorageBackendResult<usize> {
        self.control.check()?;
        let limit = output.len().min(16 * 1024);
        let mut written = 0;
        while written < limit {
            if let Some(values) = &mut self.values {
                let count =
                    values
                        .read(&mut output[written..limit])
                        .map_err(|error| match error {
                            uqa_core::value_image::ValueImageError::Memory(error) => error.into(),
                            uqa_core::value_image::ValueImageError::Cancelled(error) => {
                                error.into()
                            }
                            other => uqa_storage::StorageBackendError::backend(
                                "owner image value",
                                other,
                            ),
                        })?;
                written += count;
                if count == 0 {
                    self.values = None;
                }
                continue;
            }
            if self.pending.is_none() {
                self.pending = self.next_piece()?;
                self.position = 0;
            }
            let Some(piece) = &self.pending else {
                if self.values.is_some() {
                    continue;
                }
                break;
            };
            let bytes = &piece.bytes()[self.position..];
            let count = bytes.len().min(limit - written);
            output[written..written + count].copy_from_slice(&bytes[..count]);
            self.position += count;
            written += count;
            if count == bytes.len() {
                self.pending = None;
            }
        }
        self.control.check()?;
        Ok(written)
    }
}

pub(super) struct GraphImageCursor<'a> {
    validation: Validation<'a>,
    encoder: Encoder<'a>,
}

impl<'a> GraphImageCursor<'a> {
    pub(super) fn new(store: &'a MemoryGraphStore, control: &'a StorageReadControl) -> Self {
        Self {
            validation: Validation::new(store),
            encoder: Encoder::new(store, control),
        }
    }
}

impl ImageRead for GraphImageCursor<'_> {
    fn prepare(&mut self, maximum_positions: usize) -> StorageBackendResult<ImagePreparation> {
        self.validation
            .advance(maximum_positions, self.encoder.control)
    }
    fn read(&mut self, output: &mut [u8]) -> StorageBackendResult<usize> {
        self.encoder.read(output)
    }
}
