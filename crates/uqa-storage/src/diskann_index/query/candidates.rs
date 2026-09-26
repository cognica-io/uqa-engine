//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, DiskANNQuery};
use crate::diskann_index::{
    format::{DiskANNCanonicalOrigin, DiskANNNode, DiskANNVectorVersion},
    scoring::selection::TopK,
    DiskANNCanonicalScorer, DiskANNDocumentScore,
};
use crate::{read_control::StorageReadControl, StorageBackendResult};
use uqa_core::{memory::BudgetedMap, DocId, PostingList};

#[derive(Clone, Copy)]
struct Candidate {
    origin: DiskANNCanonicalOrigin,
    score: Option<DiskANNDocumentScore>,
}

impl Candidate {
    fn validate(
        self,
        ordinal: u32,
        version: DiskANNVectorVersion,
    ) -> StorageBackendResult<Option<DiskANNDocumentScore>> {
        if self.origin.version() != version || u64::from(ordinal) >= self.origin.count() {
            return Err(invalid(
                "physical candidate differs from its generation origin",
            ));
        }
        Ok(self.score)
    }
}

pub(super) struct Candidates<'a, 's> {
    query: &'a DiskANNQuery<'s>,
    scorer: DiskANNCanonicalScorer<'a>,
    selected: TopK,
    graph: BudgetedMap<DocId, Candidate>,
    control: &'a StorageReadControl,
}

impl<'a, 's> Candidates<'a, 's> {
    pub(super) fn new(
        query: &'a DiskANNQuery<'s>,
        scorer: DiskANNCanonicalScorer<'a>,
        k: usize,
        control: &'a StorageReadControl,
    ) -> Self {
        Self {
            query,
            scorer,
            selected: TopK::deduplicating(k, control),
            graph: BudgetedMap::new(control.memory()),
            control,
        }
    }

    pub(super) fn len(&self) -> usize {
        self.selected.len()
    }

    fn candidate(&self, document: DocId) -> StorageBackendResult<Candidate> {
        self.query.check(self.control)?;
        let origin = self
            .query
            .origins
            .origin(document, self.control)?
            .ok_or_else(|| invalid("physical document has no generation origin"))?;
        if origin.count() == 0 {
            return Err(invalid("physical candidate belongs to an empty tensor"));
        }
        let score = self.scorer.score_candidate(document, 0, origin.version())?;
        if score.is_some_and(|score| score.vector_count() != origin.count()) {
            return Err(invalid(
                "canonical tensor count differs from its original version",
            ));
        }
        Ok(Candidate { origin, score })
    }

    pub(super) fn nodes(&mut self, nodes: &[DiskANNNode]) -> StorageBackendResult<()> {
        for node in nodes {
            self.query.check(self.control)?;
            let candidate = if let Some(candidate) = self.graph.get(&node.doc_id()) {
                *candidate
            } else {
                let candidate = self.candidate(node.doc_id())?;
                self.graph.insert(node.doc_id(), candidate)?;
                candidate
            };
            if let Some(score) = candidate.validate(node.ordinal(), node.version())? {
                self.selected.offer(score)?;
            }
        }
        Ok(())
    }

    pub(super) fn side(&mut self) -> StorageBackendResult<()> {
        // Side identities are document ordered; only one additional tensor score needs retention, even for a large numeric-only corpus.
        let mut previous: Option<(DocId, Candidate)> = None;
        self.query.reader.visit_side(self.control, &mut |entry| {
            self.query.check(self.control)?;
            let candidate = match previous {
                Some((document, candidate)) if document == entry.doc_id() => candidate,
                _ => self
                    .graph
                    .get(&entry.doc_id())
                    .copied()
                    .map_or_else(|| self.candidate(entry.doc_id()), Ok)?,
            };
            if let Some(score) = candidate.validate(entry.ordinal(), entry.version())? {
                self.selected.offer(score)?;
            }
            previous = Some((entry.doc_id(), candidate));
            Ok(())
        })
    }

    pub(super) fn changes(&mut self) -> StorageBackendResult<()> {
        let mut after = None;
        while let Some(change) = self
            .query
            .canonical
            .next_change_after(after, self.control)?
        {
            self.query.check(self.control)?;
            let document = change.document();
            if after.is_some_and(|previous| document <= previous) {
                return Err(invalid("query change cursor did not advance"));
            }
            after = Some(document);
            if self.query.canonical.origin(document, self.control)? != Some(change.version()) {
                return Err(invalid("query change differs from its canonical origin"));
            }
            if self
                .query
                .origins
                .origin(document, self.control)?
                .is_some_and(|origin| origin.version() == change.version())
            {
                continue;
            }
            if let Some(score) = self.scorer.score_document(document)? {
                if score.version() != change.version() {
                    return Err(invalid("canonical change moved while scoring"));
                }
                self.selected.offer(score)?;
            }
        }
        self.query.check(self.control)
    }

    pub(super) fn finish(self) -> StorageBackendResult<PostingList> {
        drop(self.graph);
        self.selected.finish(self.control)
    }
}
