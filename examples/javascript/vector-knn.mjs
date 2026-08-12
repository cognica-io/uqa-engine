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

  const results = { exact: await knn(engine) };
  await engine.sql("CREATE INDEX notes_embedding_hnsw ON notes USING hnsw (embedding)");
  results.hnsw = await knn(engine);
  await engine.sql("DROP INDEX notes_embedding_hnsw");
  await engine.sql(
    "CREATE INDEX notes_embedding_ivf ON notes USING ivf (embedding) " +
      "WITH (lists = 2, probes = 2, train_threshold = 4)",
  );
  results.ivf = await knn(engine);
  results.filtered = (
    await engine.sql(
      "SELECT id, title, topic FROM notes " +
        "WHERE knn_match(embedding, ARRAY[1.0, 0.0, 0.0, 0.0], 6) " +
        "AND topic = 'cooking' LIMIT 3",
    )
  ).rows;

  for (const method of ["exact", "hnsw", "ivf"]) {
    if (results[method][0]?.id !== 1) {
      throw new Error(`${method} KNN did not rank document 1 first`);
    }
  }
  assertEqual(
    results.filtered.map((row) => row.topic),
    ["cooking", "cooking", "cooking"],
    "filtered KNN",
  );
  return results;
}

async function knn(engine) {
  return (
    await engine.sql(
      "SELECT id, title, topic FROM notes " +
        "WHERE knn_match(embedding, ARRAY[1.0, 0.0, 0.0, 0.0], 3)",
    )
  ).rows;
}
