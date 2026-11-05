//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate primary/derived ownership with resumable borrowed positions.

use super::super::native::{NativeMapIter, NativeSetIter};
use super::super::NativeSet;
use super::{invalid, MemoryGraphStore, Partition, StorageBackendResult, StorageReadControl};
use uqa_core::{Edge, Vertex};
use uqa_storage::image_io::ImagePreparation;

pub(super) fn validate(
    store: &MemoryGraphStore,
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let mut cursor = Validation::new(store);
    while !cursor.advance(128, control)?.ready {}
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Counters,
    Vertices,
    Edges,
    Graphs,
    VertexMembership,
    EdgeMembership,
    Complete,
}

pub(super) struct Validation<'a> {
    store: &'a MemoryGraphStore,
    stage: Stage,
    vertices: NativeMapIter<'a, u64, Vertex>,
    edges: NativeMapIter<'a, u64, Edge>,
    graphs: NativeMapIter<'a, String, Partition>,
    partition: Option<PartitionValidation<'a>>,
    vertex_membership: NativeMapIter<'a, u64, NativeSet<String>>,
    edge_membership: NativeMapIter<'a, u64, NativeSet<String>>,
    names: Option<(u64, NativeSetIter<'a, String>)>,
    #[cfg(test)]
    pub(super) visited: usize,
}

impl<'a> Validation<'a> {
    pub(super) fn new(store: &'a MemoryGraphStore) -> Self {
        Self {
            store,
            stage: Stage::Counters,
            vertices: store.vertices.iter(),
            edges: store.edges.iter(),
            graphs: store.graphs.iter(),
            partition: None,
            vertex_membership: store.vertex_membership.iter(),
            edge_membership: store.edge_membership.iter(),
            names: None,
            #[cfg(test)]
            visited: 0,
        }
    }

    pub(super) fn advance(
        &mut self,
        maximum: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<ImagePreparation> {
        control.check()?;
        let mut positions = 0;
        for _ in 0..maximum {
            if self.stage == Stage::Complete {
                break;
            }
            control.check()?;
            self.step()?;
            positions += 1;
            #[cfg(test)]
            {
                self.visited += 1;
            }
        }
        control.check()?;
        Ok(ImagePreparation {
            positions,
            ready: self.stage == Stage::Complete,
        })
    }

    fn step(&mut self) -> StorageBackendResult<()> {
        let store = self.store;
        match self.stage {
            Stage::Counters => {
                if store
                    .vertices
                    .last_key_value()
                    .is_some_and(|(id, _)| *id >= store.next_vertex_id)
                    || store
                        .edges
                        .last_key_value()
                        .is_some_and(|(id, _)| *id >= store.next_edge_id)
                {
                    return Err(invalid());
                }
                self.stage = Stage::Vertices;
            }
            Stage::Vertices => match self.vertices.next() {
                Some((id, vertex)) if *id != vertex.vertex_id => return Err(invalid()),
                Some(_) => {}
                None => self.stage = Stage::Edges,
            },
            Stage::Edges => match self.edges.next() {
                Some((id, edge)) if *id != edge.edge_id => return Err(invalid()),
                Some(_) => {}
                None => self.stage = Stage::Graphs,
            },
            Stage::Graphs => {
                if let Some(partition) = &mut self.partition {
                    if partition.step(store)? {
                        self.partition = None;
                    }
                } else if let Some((name, part)) = self.graphs.next() {
                    self.partition = Some(PartitionValidation::new(name, part));
                } else {
                    self.stage = Stage::VertexMembership;
                }
            }
            Stage::VertexMembership | Stage::EdgeMembership => {
                let vertex = self.stage == Stage::VertexMembership;
                if let Some((id, names)) = &mut self.names {
                    if let Some(name) = names.next() {
                        let exists = if vertex {
                            store.vertices.contains_key(id)
                        } else {
                            store.edges.contains_key(id)
                        };
                        if !exists
                            || !store.graphs.get(name).is_some_and(|part| {
                                if vertex {
                                    part.vertex_ids.contains(id)
                                } else {
                                    part.edge_ids.contains(id)
                                }
                            })
                        {
                            return Err(invalid());
                        }
                    } else {
                        self.names = None;
                    }
                } else {
                    let next = if vertex {
                        self.vertex_membership.next()
                    } else {
                        self.edge_membership.next()
                    };
                    if let Some((id, names)) = next {
                        self.names = Some((*id, names.iter()));
                    } else {
                        self.stage = if vertex {
                            Stage::EdgeMembership
                        } else {
                            Stage::Complete
                        };
                    }
                }
            }
            Stage::Complete => unreachable!("completed graph validation"),
        }
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PartitionStage {
    Vertices,
    Edges,
    VertexLabels,
    EdgeLabels,
    Outgoing,
    Incoming,
    Complete,
}

struct PartitionValidation<'a> {
    name: &'a str,
    part: &'a Partition,
    stage: PartitionStage,
    vertices: NativeSetIter<'a, u64>,
    edges: NativeSetIter<'a, u64>,
    vertex_labels: NativeMapIter<'a, String, NativeSet<u64>>,
    edge_labels: NativeMapIter<'a, String, NativeSet<u64>>,
    outgoing: NativeMapIter<'a, u64, NativeSet<u64>>,
    incoming: NativeMapIter<'a, u64, NativeSet<u64>>,
    label: &'a str,
    endpoint: u64,
    ids: Option<NativeSetIter<'a, u64>>,
}

impl<'a> PartitionValidation<'a> {
    fn new(name: &'a str, part: &'a Partition) -> Self {
        Self {
            name,
            part,
            stage: PartitionStage::Vertices,
            vertices: part.vertex_ids.iter(),
            edges: part.edge_ids.iter(),
            vertex_labels: part.vertex_label_index.iter(),
            edge_labels: part.label_index.iter(),
            outgoing: part.adj_out.iter(),
            incoming: part.adj_in.iter(),
            label: "",
            endpoint: 0,
            ids: None,
        }
    }

    fn step(&mut self, store: &MemoryGraphStore) -> StorageBackendResult<bool> {
        let part = self.part;
        match self.stage {
            PartitionStage::Vertices => {
                if let Some(id) = self.vertices.next() {
                    let vertex = store.vertices.get(id).ok_or_else(invalid)?;
                    if !store
                        .vertex_membership
                        .get(id)
                        .is_some_and(|names| names.contains(self.name))
                        || !part
                            .vertex_label_index
                            .get(&vertex.label)
                            .is_some_and(|ids| ids.contains(id))
                    {
                        return Err(invalid());
                    }
                } else {
                    self.stage = PartitionStage::Edges;
                }
            }
            PartitionStage::Edges => {
                if let Some(id) = self.edges.next() {
                    let edge = store.edges.get(id).ok_or_else(invalid)?;
                    if !store
                        .edge_membership
                        .get(id)
                        .is_some_and(|names| names.contains(self.name))
                        || !part
                            .label_index
                            .get(&edge.label)
                            .is_some_and(|ids| ids.contains(id))
                        || !part
                            .adj_out
                            .get(&edge.source_id)
                            .is_some_and(|ids| ids.contains(id))
                        || !part
                            .adj_in
                            .get(&edge.target_id)
                            .is_some_and(|ids| ids.contains(id))
                    {
                        return Err(invalid());
                    }
                } else {
                    self.stage = PartitionStage::VertexLabels;
                }
            }
            PartitionStage::VertexLabels | PartitionStage::EdgeLabels => self.step_labels(store)?,
            PartitionStage::Outgoing | PartitionStage::Incoming => {
                if let Some(ids) = &mut self.ids {
                    if let Some(id) = ids.next() {
                        let edge = store.edges.get(id).ok_or_else(invalid)?;
                        if !part.edge_ids.contains(id)
                            || self.endpoint
                                != if self.stage == PartitionStage::Outgoing {
                                    edge.source_id
                                } else {
                                    edge.target_id
                                }
                        {
                            return Err(invalid());
                        }
                    } else {
                        self.ids = None;
                    }
                } else {
                    let next = if self.stage == PartitionStage::Outgoing {
                        self.outgoing.next()
                    } else {
                        self.incoming.next()
                    };
                    if let Some((endpoint, ids)) = next {
                        self.endpoint = *endpoint;
                        self.ids = Some(ids.iter());
                    } else {
                        self.stage = if self.stage == PartitionStage::Outgoing {
                            PartitionStage::Incoming
                        } else {
                            PartitionStage::Complete
                        };
                    }
                }
            }
            PartitionStage::Complete => unreachable!("completed partition validation"),
        }
        Ok(self.stage == PartitionStage::Complete)
    }
    fn step_labels(&mut self, store: &MemoryGraphStore) -> StorageBackendResult<()> {
        let part = self.part;

        if let Some(ids) = &mut self.ids {
            if let Some(id) = ids.next() {
                let valid = if self.stage == PartitionStage::VertexLabels {
                    part.vertex_ids.contains(id)
                        && store
                            .vertices
                            .get(id)
                            .is_some_and(|v| v.label == self.label)
                } else {
                    part.edge_ids.contains(id)
                        && store.edges.get(id).is_some_and(|e| e.label == self.label)
                };
                if !valid {
                    return Err(invalid());
                }
            } else {
                self.ids = None;
            }
        } else {
            let next = if self.stage == PartitionStage::VertexLabels {
                self.vertex_labels.next()
            } else {
                self.edge_labels.next()
            };
            if let Some((label, ids)) = next {
                self.label = label;
                self.ids = Some(ids.iter());
            } else {
                self.stage = if self.stage == PartitionStage::VertexLabels {
                    PartitionStage::EdgeLabels
                } else {
                    PartitionStage::Outgoing
                };
            }
        }

        Ok(())
    }
}
