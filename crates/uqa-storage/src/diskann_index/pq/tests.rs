//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use proptest::prelude::*;
use serde_json::Value;

use super::*;
use crate::diskann_index::NavigationInput;

mod resources;

fn navigation(raw: &[f32], control: &StorageReadControl) -> NavigationVector {
    match NavigationInput::from_raw(raw.len() as u32, raw, control).unwrap() {
        NavigationInput::Navigable(vector) => vector,
        NavigationInput::Exact(reason) => panic!("unexpected exact vector: {reason:?}"),
    }
}

fn options(centroids: u16) -> PQTrainingOptions {
    PQTrainingOptions {
        max_centroids: centroids,
        ..PQTrainingOptions::default()
    }
}

fn numbers(value: &Value) -> Vec<f64> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_f64().unwrap())
        .collect()
}

fn rational(value: &Value) -> f64 {
    let text = value.as_str().unwrap();
    if let Some((numerator, denominator)) = text.split_once('/') {
        numerator.parse::<f64>().unwrap() / denominator.parse::<f64>().unwrap()
    } else {
        text.parse().unwrap()
    }
}

fn fixture_codebook(control: &StorageReadControl) -> (PQCodebook, Value) {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/diskann/reference.json"
    ))
    .unwrap();
    let pq = fixture["pq"].clone();
    let mut centroids = BudgetedVec::new(control.memory());
    for chunk in pq["codebooks"].as_array().unwrap() {
        for centroid in chunk.as_array().unwrap() {
            centroids.extend_from_slice(&numbers(centroid)).unwrap();
        }
    }
    (
        PQCodebook {
            dimensions: 5,
            pq_bytes: 2,
            centroid_count: 2,
            centroids,
            training: PQTrainingSummary {
                options: options(2),
                observed_vectors: 2,
                sampled_vectors: 2,
            },
        },
        pq,
    )
}

#[test]
fn independent_lookup_distances_and_tie_labels_match() {
    let control = StorageReadControl::with_limit(4096);
    let (book, fixture) = fixture_codebook(&control);
    assert_eq!(book.chunk_range(0), Some(0..3));
    assert_eq!(book.chunk_range(1), Some(3..5));
    assert_eq!(book.chunk_range(2), None);
    let lookup = book
        .lookup_coordinates(&numbers(&fixture["query"]), &control)
        .unwrap();
    let expected: Vec<_> = fixture["lookup"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(numbers)
        .collect();
    assert_eq!(&*lookup.distances, expected);
    for (code, expected) in fixture["codes"]
        .as_array()
        .unwrap()
        .iter()
        .zip(numbers(&fixture["distances"]))
    {
        let code: Vec<_> = numbers(code).iter().map(|&x| x as u8).collect();
        assert_eq!(lookup.estimate(&code, &control).unwrap().get(), expected);
    }
    let code = book
        .encode_coordinates(&numbers(&fixture["tie_query"]), &control)
        .unwrap();
    assert_eq!(&*code, &[0, 0]);
    for bad in [vec![], vec![0], vec![0, 0, 0], vec![2, 0], vec![0, 255]] {
        assert!(lookup.estimate(&bad, &control).is_err());
    }
}

#[test]
fn independent_reservoir_training_encoding_and_lookup_match_rational_oracle() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/diskann/training.json"
    ))
    .unwrap();
    let control = StorageReadControl::with_limit(16_384);
    let options = PQTrainingOptions {
        max_samples: 5,
        ..options(2)
    };
    let rows: Vec<Vec<f32>> = fixture["vectors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| numbers(row).iter().map(|&x| x as f32).collect())
        .collect();
    let mut previous = None;
    for _ in 0..2 {
        let mut trainer = PQTrainer::new(5, 2, options, &control).unwrap();
        for row in &rows {
            trainer.observe(&navigation(row, &control)).unwrap();
        }
        assert_eq!(trainer.observed_vectors(), 8);
        assert_eq!(trainer.sampled_vectors(), 5);
        for (sample, id) in trainer.samples.iter().zip(numbers(&fixture["sample_ids"])) {
            assert_eq!(
                &**sample,
                rows[id as usize]
                    .iter()
                    .map(|&x| f64::from(x))
                    .collect::<Vec<_>>()
            );
        }
        let book = trainer.finish().unwrap();
        assert_eq!(
            book.training(),
            PQTrainingSummary {
                options,
                observed_vectors: 8,
                sampled_vectors: 5
            }
        );
        assert_eq!(
            (PQCodebook::CODEC_REVISION, PQCodebook::TRAINING_REVISION),
            (1, 1)
        );
        for (chunk, expected) in fixture["codebooks"].as_array().unwrap().iter().enumerate() {
            let expected = expected
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|centroid| centroid.as_array().unwrap().iter().map(rational));
            for (&actual, expected) in book.chunk_centroids(chunk).unwrap().iter().zip(expected) {
                assert!((actual - expected).abs() < 1.0e-15);
            }
        }
        let lookup = book
            .lookup(&navigation(&[1.0, 0.0, 0.0, 0.0, 0.0], &control), &control)
            .unwrap();
        for (index, row) in rows.iter().enumerate() {
            let code = book.encode(&navigation(row, &control), &control).unwrap();
            assert_eq!(
                &*code,
                numbers(&fixture["codes"][index])
                    .iter()
                    .map(|&x| x as u8)
                    .collect::<Vec<_>>()
            );
            assert!(
                (lookup.estimate(&code, &control).unwrap().get()
                    - rational(&fixture["distances"][index]))
                .abs()
                    < 1.0e-15
            );
        }
        let bits: Vec<_> = book.centroids.iter().map(|x| x.to_bits()).collect();
        if let Some(previous) = previous.replace(bits.clone()) {
            assert_eq!(previous, bits);
        }
    }
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn euclidean_means_are_not_spherically_renormalized() {
    let control = StorageReadControl::with_limit(4096);
    let mut trainer = PQTrainer::new(5, 2, options(1), &control).unwrap();
    for row in [[1.0, 0.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0, 0.0]] {
        trainer.observe(&navigation(&row, &control)).unwrap();
    }
    let book = trainer.finish().unwrap();
    assert_eq!(book.chunk_centroids(0), Some([0.5, 0.5, 0.0].as_slice()));
    assert_eq!(book.chunk_centroids(1), Some([0.0, 0.0].as_slice()));
}

#[test]
fn tiny_samples_and_empty_clusters_keep_actual_centroid_counts_and_valid_byte_labels() {
    let control = StorageReadControl::with_limit(1 << 20);
    let vector = navigation(&[1.0], &control);
    for count in [1, 2, 255, 256, 257] {
        let mut trainer = PQTrainer::new(1, 1, options(256), &control).unwrap();
        for _ in 0..count {
            trainer.observe(&vector).unwrap();
        }
        let book = trainer.finish().unwrap();
        assert_eq!(book.centroid_count(), count.min(256));
        assert!(book.centroids.iter().all(|&value| value == 1.0));
        assert_eq!(&*book.encode(&vector, &control).unwrap(), &[0]);
        let lookup = book.lookup(&vector, &control).unwrap();
        assert_eq!(lookup.estimate(&[0], &control).unwrap().get(), 0.0);
        if count >= 256 {
            assert_eq!(lookup.estimate(&[255], &control).unwrap().get(), 0.0);
        } else {
            assert!(lookup.estimate(&[count as u8], &control).is_err());
        }
    }
}

#[test]
fn invalid_training_shapes_limits_and_query_dimensions_are_rejected() {
    let control = StorageReadControl::with_limit(4096);
    for (dimensions, chunks) in [(0, 1), (2, 0), (2, 3)] {
        assert!(PQTrainer::new(dimensions, chunks, options(2), &control).is_err());
    }
    for bad in [
        PQTrainingOptions {
            max_samples: 0,
            ..options(2)
        },
        PQTrainingOptions {
            max_iterations: 0,
            ..options(2)
        },
        PQTrainingOptions {
            max_iterations: 257,
            ..options(2)
        },
        options(0),
        options(257),
    ] {
        assert!(PQTrainer::new(2, 1, bad, &control).is_err());
    }
    assert!(PQTrainer::new(2, 1, options(2), &control)
        .unwrap()
        .finish()
        .is_err());
    let mut trainer = PQTrainer::new(2, 1, options(2), &control).unwrap();
    let wrong = navigation(&[1.0], &control);
    assert!(trainer.observe(&wrong).is_err());
    trainer.observe(&navigation(&[1.0, 0.0], &control)).unwrap();
    let book = trainer.finish().unwrap();
    assert!(book.encode(&wrong, &control).is_err());
    assert!(book.lookup(&wrong, &control).is_err());
}

proptest! {
    #[test]
    fn pq_centroids_stay_in_coordinate_hulls_and_lookup_is_squared_reconstruction_distance(
        rows in prop::collection::vec(prop::array::uniform5(-100_i16..=100), 1..8),
        seed in any::<u64>(),
        chunks in 1_usize..=5,
    ) {
        prop_assume!(rows.iter().all(|row| row.iter().any(|&x| x != 0)));
        let control = StorageReadControl::with_limit(65_536);
        let rows: Vec<_> = rows.iter().map(|row| navigation(&row.map(f32::from), &control)).collect();
        let mut trainer = PQTrainer::new(5, chunks, PQTrainingOptions { seed, ..options(2) }, &control).unwrap();
        for row in &rows { trainer.observe(row).unwrap(); }
        let book = trainer.finish().unwrap();
        for chunk in 0..chunks {
            let range = book.chunk_range(chunk).unwrap();
            for centroid in book.chunk_centroids(chunk).unwrap().chunks_exact(range.len()) {
                for (offset, &value) in centroid.iter().enumerate() {
                    let coordinate = range.start + offset;
                    let minimum = rows.iter().map(|row| row.coordinates()[coordinate]).fold(f64::INFINITY, f64::min);
                    let maximum = rows.iter().map(|row| row.coordinates()[coordinate]).fold(f64::NEG_INFINITY, f64::max);
                    prop_assert!(value >= minimum - 1.0e-15 && value <= maximum + 1.0e-15);
                }
            }
        }
        let lookup = book.lookup(&rows[0], &control).unwrap();
        for row in &rows {
            let code = book.encode(row, &control).unwrap();
            let mut expected = 0.0;
            for (chunk, &label) in code.iter().enumerate() {
                let range = book.chunk_range(chunk).unwrap();
                let centroids = book.chunk_centroids(chunk).unwrap();
                let centroid = &centroids[usize::from(label) * range.len()..(usize::from(label) + 1) * range.len()];
                for (&query, &center) in rows[0].coordinates()[range].iter().zip(centroid) {
                    expected += (query - center).powi(2);
                }
            }
            let actual = lookup.estimate(&code, &control).unwrap().get();
            prop_assert!(actual >= 0.0 && (actual - expected).abs() < 1.0e-14);
        }
    }
}
