//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

import { assertEqual } from "./common.mjs";

const CORPUS = [
  [1, "async runtimes", "systems", [0.95, 0.1, 0.05, 0]],
  [2, "ownership and borrows", "systems", [0.9, 0.2, 0, 0.1]],
  [3, "zero-copy parsing", "systems", [0.85, 0.05, 0.15, 0.05]],
  [4, "sourdough starters", "cooking", [0.05, 0.95, 0.1, 0]],
  [5, "knife skills", "cooking", [0, 0.9, 0.2, 0.05]],
  [6, "fermentation basics", "cooking", [0.1, 0.85, 0.05, 0.15]],
];

export async function runVectorKNN(engine, vector) {
  await engine.sql(
    "CREATE TABLE notes (" +
      "id INTEGER PRIMARY KEY, title TEXT, topic TEXT, embedding VECTOR(4))",
  );
  for (const [id, title, topic, embedding] of CORPUS) {
    await engine.sql(
      "INSERT INTO notes (id, title, topic, embedding) VALUES ($1, $2, $3, $4)",
      [id, title, topic, vector(embedding)],
    );
  }

  const results = { exact: await knn(engine, vector) };
  await engine.sql("CREATE INDEX notes_embedding_hnsw ON notes USING hnsw (embedding)");
  results.hnsw = await knn(engine, vector);
  await engine.sql("DROP INDEX notes_embedding_hnsw");
  await engine.sql(
    "CREATE INDEX notes_embedding_ivf ON notes USING ivf (embedding) " +
      "WITH (lists = 2, probes = 2, train_threshold = 4)",
  );
  results.ivf = await knn(engine, vector);
  await engine.sql("DROP INDEX notes_embedding_ivf");
  await engine.sql(
    "CREATE INDEX notes_embedding_diskann ON notes USING diskann (embedding) " +
      "WITH (max_degree = 4, search_list_size = 16, beam_width = 2)",
  );
  results.diskann = await knn(engine, vector);
  assertEqual(results.diskann, results.exact, "DiskANN canonical scores and rows");
  assertEqual(results.diskann.map((row) => row.id), [1, 3, 2], "DiskANN top three");
  results.filtered = (
    await engine.sql(
      "SELECT id, title, topic FROM notes " +
        "WHERE knn_match(embedding, ARRAY[1.0, 0.0, 0.0, 0.0], 6) " +
        "AND topic = 'cooking' ORDER BY _score DESC, id LIMIT 3",
    )
  ).rows;

  for (const method of ["exact", "hnsw", "ivf", "diskann"]) {
    if (results[method][0]?.id !== 1) {
      throw new Error(`${method} KNN did not rank document 1 first`);
    }
  }
  assertEqual(
    results.filtered.map((row) => row.topic),
    ["cooking", "cooking", "cooking"],
    "filtered KNN",
  );
  await engine.sql("BEGIN");
  await engine.sql("UPDATE notes SET embedding = $1 WHERE id = 6", [vector([1, 0, 0, 0])]);
  const privateRows = await knn(engine, vector);
  assertEqual([privateRows[0].id, privateRows[0]._score], [6, 1], "private DiskANN replacement");
  await engine.sql("ROLLBACK");
  results.afterRollback = await knn(engine, vector);
  assertEqual(results.afterRollback, results.exact, "DiskANN rollback");
  await engine.sql("UPDATE notes SET embedding = $1 WHERE id = 6", [vector([1, 0, 0, 0])]);
  results.afterCommit = await knn(engine, vector, 6);
  assertEqual(results.afterCommit.map(({ id }) => id), [6, 1, 3, 2, 4, 5], "complete committed candidate pool");
  assertEqual([results.afterCommit[0].id, results.afterCommit[0]._score], [6, 1], "committed DiskANN replacement");
  return results;
}

export async function verifyVectorKNNReopen(engine, vector, expected) {
  const rows = await knn(engine, vector, 6);
  assertEqual(rows, expected, "reopened DiskANN scores and rows");
  const method = (await engine.sql(
    "SELECT indexdef FROM pg_indexes WHERE indexname = 'notes_embedding_diskann'",
  )).rows[0]?.indexdef;
  assertEqual(method?.toLowerCase().includes("using diskann"), true, "reopened DiskANN definition");
  return rows;
}

async function knn(engine, vector, k = 3) {
  return (
    await engine.sql(
      "SELECT id, title, topic, _score FROM notes " +
        "WHERE knn_match(embedding, $1, $2) " +
        "ORDER BY _score DESC, id",
      [vector([1, 0, 0, 0]), k],
    )
  ).rows;
}
