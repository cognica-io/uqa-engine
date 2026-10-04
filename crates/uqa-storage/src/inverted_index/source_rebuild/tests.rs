//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::clustered_postings::{encode_occurrence_cluster, encode_term_keys, OccurrencePosting};
use crate::inverted_index::analyze_index_field;

type Terms = BTreeMap<TokenTermKey, Vec<TokenOccurrence>>;
/// A visited cluster: its field, term key bytes, cluster, score and positions.
type Cluster = (String, Vec<u8>, u64, Vec<u8>, Vec<u8>);

fn analyzed(text: &str) -> (IndexedFieldMetadata, Terms) {
    let analyzer = uqa_analysis::whitespace_analyzer().compile().unwrap();
    let field = analyze_index_field(&analyzer, text).unwrap();
    (IndexedFieldMetadata::new(&analyzer, &field), field.terms)
}

fn stage(rebuild: &mut SourceRebuild, doc_id: DocId, fields: &[(&str, &str)]) {
    let analyzed = fields
        .iter()
        .map(|(field, text)| (*field, analyzed(text)))
        .collect::<Vec<_>>();
    rebuild
        .stage(
            doc_id,
            analyzed
                .iter()
                .map(|(field, (metadata, terms))| (*field, metadata, terms)),
        )
        .unwrap();
}

fn clusters(rebuild: &SourceRebuild) -> Vec<Cluster> {
    let mut clusters = Vec::new();
    rebuild
        .visit_clusters(&mut |cluster| {
            clusters.push((
                cluster.field.to_owned(),
                cluster.term.as_bytes().to_vec(),
                cluster.cluster,
                cluster.score.to_vec(),
                cluster.positions.to_vec(),
            ));
            Ok(())
        })
        .unwrap();
    clusters
}

#[test]
fn staged_documents_and_clusters_come_back_as_the_index_orders_them() {
    let control = StorageReadControl::with_limit(8 << 20);
    let mut rebuild = SourceRebuild::new(&control);
    let late = crate::clustered_postings::POSTING_CLUSTER_DOCS + 5;
    stage(
        &mut rebuild,
        3,
        &[("title", "beta"), ("body", "alpha beta alpha")],
    );
    stage(&mut rebuild, 5, &[("body", "beta")]);
    stage(&mut rebuild, late, &[("body", "alpha")]);
    let mut documents = Vec::new();
    rebuild
        .visit_documents(&mut |record| {
            documents.push((
                record.doc_id,
                record.field.to_owned(),
                record.metadata.length,
                record.terms.to_vec(),
            ));
            Ok(())
        })
        .unwrap();
    let terms = |words: &[&str]| {
        encode_term_keys(
            &words
                .iter()
                .map(|word| TokenTermKey::from_text(word))
                .collect::<Vec<_>>(),
        )
        .unwrap()
    };
    assert_eq!(
        documents,
        [
            (3, "body".to_owned(), 3, terms(&["alpha", "beta"])),
            (3, "title".to_owned(), 1, terms(&["beta"])),
            (5, "body".to_owned(), 1, terms(&["beta"])),
            (late, "body".to_owned(), 1, terms(&["alpha"])),
        ]
    );
    let posting = |doc_id: DocId, text: &str, term: &str| {
        let (metadata, terms) = analyzed(text);
        OccurrencePosting {
            doc_id,
            doc_length: metadata.length,
            occurrences: terms[&TokenTermKey::from_text(term)].clone(),
        }
    };
    let expected = |field: &str, term: &str, cluster: u64, entries: &[OccurrencePosting]| {
        let (score, positions) = encode_occurrence_cluster(entries).unwrap();
        (
            field.to_owned(),
            TokenTermKey::from_text(term).as_bytes().to_vec(),
            cluster,
            score,
            positions,
        )
    };
    assert_eq!(
        clusters(&rebuild),
        [
            expected(
                "body",
                "alpha",
                0,
                &[posting(3, "alpha beta alpha", "alpha")]
            ),
            expected("body", "alpha", 1, &[posting(late, "alpha", "alpha")]),
            expected(
                "body",
                "beta",
                0,
                &[
                    posting(3, "alpha beta alpha", "beta"),
                    posting(5, "beta", "beta")
                ]
            ),
            expected("title", "beta", 0, &[posting(3, "beta", "beta")]),
        ]
    );
    assert_eq!(rebuild.totals()["body"].doc_count, 3);
    assert_eq!(rebuild.totals()["body"].total_length, 5);
    assert_eq!(rebuild.totals()["title"].doc_count, 1);
}

#[test]
fn documents_out_of_identity_order_are_rejected() {
    let control = StorageReadControl::with_limit(1 << 20);
    let mut rebuild = SourceRebuild::new(&control);
    stage(&mut rebuild, 7, &[("body", "alpha")]);
    let (metadata, terms) = analyzed("beta");
    for doc_id in [7, 6] {
        assert!(rebuild
            .stage(doc_id, [("body", &metadata, &terms)])
            .is_err());
    }
}

#[test]
fn a_rebuild_larger_than_its_allowance_stages_within_it() {
    let control = StorageReadControl::with_limit(2 << 20);
    let mut rebuild = SourceRebuild::new(&control);
    let documents = 20_000;
    for doc_id in 1..=documents {
        stage(
            &mut rebuild,
            doc_id,
            &[("body", &format!("common unique{doc_id}"))],
        );
    }
    let mut common = 0;
    let mut unique = 0;
    rebuild
        .visit_clusters(&mut |cluster| {
            if *cluster.term == TokenTermKey::from_text("common") {
                common += crate::clustered_postings::score_count(cluster.score)?;
            } else {
                unique += 1;
            }
            Ok(())
        })
        .unwrap();
    assert_eq!((common, unique), (documents, documents));
    assert!(control.memory().peak() <= control.memory().limit());
    drop(rebuild);
    assert_eq!(control.memory().used(), 0);
}
