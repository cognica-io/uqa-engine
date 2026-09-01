//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

import { assertEqual } from "./common.mjs";

const GRAPH = "citations";
const CURRENT_YEAR = 2026;
const PAPERS = [
  [
    1,
    "Learned sparse retrieval at scale",
    "sparse retrieval with learned term weights and inverted index pruning for ranking",
    "SIGIR",
    2024,
    [0.95, 0.1, 0.05],
  ],
  [
    2,
    "Block-max pruning revisited",
    "dynamic pruning for inverted index retrieval with block max bounds and ranking",
    "SIGIR",
    2019,
    [0.9, 0.15, 0],
  ],
  [
    3,
    "Vector quantization for dense retrieval",
    "dense retrieval with product quantization compressing embeddings for ranking",
    "NeurIPS",
    2025,
    [0.8, 0.35, 0.05],
  ],
  [
    4,
    "LSM trees under write amplification",
    "storage engines log structured merge trees and write amplification tradeoffs",
    "VLDB",
    2023,
    [0.05, 0.95, 0.1],
  ],
  [
    5,
    "Graph pattern matching in RDBMS",
    "graph pattern matching compiled into relational plans with worst case optimal joins",
    "VLDB",
    2025,
    [0.1, 0.25, 0.95],
  ],
  [
    6,
    "Regular path queries on property graphs",
    "regular path queries automata evaluation over property graphs and traversal",
    "ICDE",
    2018,
    [0.05, 0.1, 0.9],
  ],
];
const ARCHIVED_PAPERS = [
  [101, "Sparse retrieval retrospective", "SIGIR", [0.93, 0.12, 0.04]],
  [102, "Dense retrieval retrospective", "NeurIPS", [0.82, 0.34, 0.06]],
  [103, "Storage engines retrospective", "VLDB", [0.08, 0.96, 0.08]],
  [104, "Graph querying retrospective", "ICDE", [0.04, 0.12, 0.94]],
];
const CITES = [
  [3, 1],
  [3, 2],
  [1, 2],
  [5, 6],
  [5, 4],
];

export async function runUnifiedSearch(engine, vector) {
  await load(engine, vector);
  await engine.registerScalarFunction(
    "recency_boost",
    (year) => {
      if (year === null) {
        return 1;
      }
      const age = Math.max(CURRENT_YEAR - year, 0);
      return Math.max(0.5 ** (age / 8), 0.25);
    },
    { volatility: "immutable", mayMutateEngine: false },
  );

  const lexical = await rows(
    engine,
    "SELECT id, title, year, _score FROM papers " +
      "WHERE text_match(abstract, 'retrieval ranking') AND year >= 2020 " +
      "ORDER BY _score DESC LIMIT 5",
  );
  const bayesian = await rows(
    engine,
    "SELECT id, title, year, _score FROM papers " +
      "WHERE bayesian_match(abstract, 'retrieval ranking') AND year >= 2020 " +
      "ORDER BY _score DESC LIMIT 5",
  );
  const vectorRows = await rows(
    engine,
    "SELECT id, title, venue FROM papers " +
      "WHERE knn_match(embedding, ARRAY[1.0, 0.0, 0.0], 3)",
  );
  const fusedExact = await rows(
    engine,
    "SELECT id, title, _score FROM papers " +
      "WHERE text_match(abstract, 'retrieval ranking') " +
      "AND knn_match(embedding, ARRAY[1.0, 0.0, 0.0], 4) " +
      "ORDER BY _score DESC LIMIT 4",
  );
  const fusedPooled = await rows(
    engine,
    "SELECT id, title, _score FROM papers " +
      "WHERE pool_positive_evidence(" +
      "bayesian_match(abstract, 'retrieval ranking'), " +
      "knn_match(embedding, ARRAY[1.0, 0.0, 0.0], 4)) " +
      "ORDER BY _score DESC LIMIT 4",
  );
  const vectorPairs = await rows(
    engine,
    "SELECT pairs.left_doc_id AS live_id, " +
      "pairs.right_doc_id AS archive_id, pairs._score AS score, " +
      "p.title AS live_title, a.title AS archive_title " +
      "FROM vector_similarity_join(" +
      "papers, knn_match(embedding, ARRAY[1.0, 0.0, 0.0], 4), " +
      "archived_papers, " +
      "knn_match(archived_embedding, ARRAY[1.0, 0.0, 0.0], 4), " +
      "0.80) AS pairs " +
      "JOIN papers AS p ON p.id = pairs.left_doc_id " +
      "JOIN archived_papers AS a ON a.id = pairs.right_doc_id " +
      "ORDER BY score DESC, live_id, archive_id LIMIT 8",
  );
  const hybridPairs = await rows(
    engine,
    "SELECT pairs.left_doc_id AS live_id, " +
      "pairs.right_doc_id AS archive_id, pairs._score AS score, " +
      "p.venue, a.title AS archive_title " +
      "FROM hybrid_join(" +
      "papers, venue IS NOT NULL " +
      "AND knn_match(embedding, ARRAY[1.0, 0.0, 0.0], 6), " +
      "archived_papers, venue IS NOT NULL " +
      "AND knn_match(archived_embedding, ARRAY[1.0, 0.0, 0.0], 4)) AS pairs " +
      "JOIN papers AS p ON p.id = pairs.left_doc_id " +
      "JOIN archived_papers AS a ON a.id = pairs.right_doc_id " +
      "ORDER BY score DESC, live_id, archive_id",
  );
  const graphDocumentPairs = await rows(
    engine,
    "SELECT pairs.left_doc_id AS vertex_id, " +
      "pairs.right_doc_id AS archive_id, a.title AS archive_title " +
      `FROM cross_paradigm_join(papers, graph_pagerank('${GRAPH}'), ` +
      "archived_papers, " +
      "venue IS NOT NULL) AS pairs " +
      "JOIN archived_papers AS a ON a.id = pairs.right_doc_id " +
      "ORDER BY vertex_id, archive_id",
  );
  const blended = await rows(
    engine,
    "SELECT id, title, year, _score, " +
      "_score * recency_boost(year) AS blended FROM papers " +
      "WHERE text_match(abstract, 'retrieval ranking') " +
      "ORDER BY blended DESC LIMIT 5",
  );
  const seed = blended[0]?.id;
  const cited = await rows(
    engine,
    "SELECT p.id, p.title, p.venue, p.year, recency_boost(p.year) AS boost " +
      "FROM papers AS p " +
      `JOIN cypher('${GRAPH}', $$ MATCH (:Paper {paper_id: ${seed}})` +
      "-[:CITES]->(cited:Paper) RETURN cited.paper_id $$) AS cited(id int) " +
      "ON p.id = cited.id WHERE p.venue <> 'ICDE' ORDER BY p.year DESC",
  );
  const reachable = await rows(
    engine,
    "SELECT id, title, year FROM papers WHERE id IN (" +
      `SELECT id FROM cypher('${GRAPH}', $$ ` +
      `MATCH (:Paper {paper_id: ${seed}})-[:CITES]->()-[:CITES]->(r:Paper) ` +
      "RETURN r.paper_id $$) AS reached(id int)) ORDER BY year DESC",
  );
  const unified = await rows(
    engine,
    "SELECT id, title, year, _score * recency_boost(year) AS blended " +
      "FROM papers WHERE text_match(abstract, 'retrieval ranking graph') " +
      "AND year >= 2018 AND id IN (" +
      `SELECT id FROM cypher('${GRAPH}', $$ ` +
      `MATCH (:Paper {paper_id: ${seed}})-[:CITES*1..2]->(r:Paper) ` +
      "RETURN r.paper_id $$) AS reached(id int)) " +
      "ORDER BY blended DESC LIMIT 5",
  );

  assertUnifiedResults({
    lexical,
    bayesian,
    vectorRows,
    fusedExact,
    fusedPooled,
    vectorPairs,
    hybridPairs,
    graphDocumentPairs,
    blended,
    cited,
    reachable,
    unified,
  });
  return {
    lexical,
    bayesian,
    vector: vectorRows,
    fusedExact,
    fusedPooled,
    vectorPairs,
    hybridPairs,
    graphDocumentPairs,
    blended,
    cited,
    reachable,
    unified,
  };
}

function assertUnifiedResults(results) {
  for (const name of [
    "lexical",
    "bayesian",
    "vectorRows",
    "fusedExact",
    "fusedPooled",
    "vectorPairs",
    "hybridPairs",
    "graphDocumentPairs",
    "blended",
    "cited",
    "reachable",
    "unified",
  ]) {
    if (results[name].length === 0) {
      throw new Error(`unified search stage ${name} returned no rows`);
    }
  }
  assertEqual(results.blended[0].id, 3, "recency-blended winner");
  assertEqual(results.vectorRows[0].id, 1, "vector winner");
  if (!results.vectorPairs.every((row) => row.live_id < 100 && row.archive_id > 100)) {
    throw new Error("vector operator join did not preserve live and archive identities");
  }
  if (!results.hybridPairs.every((row) => row.live_id < 100 && row.archive_id > 100)) {
    throw new Error("hybrid operator join did not preserve live and archive identities");
  }
  if (!results.graphDocumentPairs.every((row) => row.archive_id > 100)) {
    throw new Error("cross-paradigm join did not preserve graph and archive identities");
  }
  assertEqual(results.cited.map((row) => row.id), [1, 2], "direct citations");
  assertEqual(results.reachable.map((row) => row.id), [2], "two-hop citations");
  assertEqual(results.unified.map((row) => row.id), [1, 2], "unified result");
}

async function rows(engine, query) {
  return (await engine.sql(query)).rows;
}

async function load(engine, vector) {
  await engine.sql(
    "CREATE TABLE papers (id INTEGER PRIMARY KEY, title TEXT, abstract TEXT, " +
      "venue TEXT, year INTEGER, embedding VECTOR(3))",
  );
  await engine.sql("CREATE INDEX papers_text_gin ON papers USING gin (title, abstract)");
  for (const [id, title, abstract, venue, year, embedding] of PAPERS) {
    await engine.sql(
      "INSERT INTO papers (id, title, abstract, venue, year, embedding) " +
        "VALUES ($1, $2, $3, $4, $5, $6)",
      [id, title, abstract, venue, year, vector(embedding)],
    );
  }
  await engine.sql("CREATE INDEX papers_embedding_hnsw ON papers USING hnsw (embedding)");
  await engine.sql(
    "CREATE TABLE archived_papers (id INTEGER PRIMARY KEY, title TEXT, " +
      "venue TEXT, archived_embedding VECTOR(3))",
  );
  for (const [id, title, venue, embedding] of ARCHIVED_PAPERS) {
    await engine.sql(
      "INSERT INTO archived_papers (id, title, venue, archived_embedding) " +
        "VALUES ($1, $2, $3, $4)",
      [id, title, venue, vector(embedding)],
    );
  }
  await engine.sql(
    "CREATE INDEX archived_papers_embedding_hnsw " +
      "ON archived_papers USING hnsw (archived_embedding)",
  );
  await engine.sql(`SELECT create_graph('${GRAPH}') AS ok`);
  for (const [id, , , venue] of PAPERS) {
    await cypher(engine, `CREATE (:Paper {paper_id: ${id}, venue: '${venue}'})`);
  }
  for (const [source, target] of CITES) {
    await cypher(
      engine,
      `MATCH (a:Paper {paper_id: ${source}}), (b:Paper {paper_id: ${target}}) ` +
        "CREATE (a)-[:CITES]->(b)",
    );
  }
}

async function cypher(engine, query) {
  await engine.sql(`SELECT * FROM cypher('${GRAPH}', $$ ${query} $$) AS (ignored agtype)`);
}
