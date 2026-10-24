/*
 * Unified Query Algebra
 *
 * Copyright (c) 2023-2026 Cognica, Inc.
 */

#include "example.h"
#include <uqa/uqa_udf.h>
#include <math.h>

static uqa_status UQA_CALL recency_boost(uqa_udf_context context,
    uqa_value_view const *arguments, uint32_t count, void *user_data) {
    (void)user_data;
    if (count != 1) return uqa_udf_context_fail(context, "recency_boost expects one year");
    uint8_t is_null = 0;
    uqa_status status = uqa_value_get_is_null(arguments[0], &is_null);
    if (status != UQA_OK) return status;
    double boost = 1;
    if (!is_null) {
        int64_t year = 0;
        status = uqa_value_get_int64(arguments[0], &year);
        if (status != UQA_OK) return status;
        double age = fmax(2026.0 - (double)year, 0);
        boost = fmax(pow(0.5, age / 8.0), 0.25);
    }
    uqa_value value = NULL;
    status = uqa_value_create_float64(boost, &value);
    if (status != UQA_OK) return status;
    status = uqa_udf_context_set_result(context, value);
    uqa_status cleanup = uqa_value_destroy(&value);
    return status == UQA_OK ? cleanup : status;
}

static void load(uqa_engine engine) {
    example_execute(engine,
        "CREATE TABLE papers (id INTEGER PRIMARY KEY, title TEXT, abstract TEXT, "
        "venue TEXT, year INTEGER, embedding VECTOR(3));"
        "CREATE INDEX papers_text_gin ON papers USING gin (title, abstract);"
        "INSERT INTO papers VALUES "
        "(1, 'Learned sparse retrieval at scale', "
        "'sparse retrieval with learned term weights and inverted index pruning for ranking', "
        "'SIGIR', 2024, ARRAY[0.95, 0.10, 0.05]),"
        "(2, 'Block-max pruning revisited', "
        "'dynamic pruning for inverted index retrieval with block max bounds and ranking', "
        "'SIGIR', 2019, ARRAY[0.90, 0.15, 0.00]),"
        "(3, 'Vector quantization for dense retrieval', "
        "'dense retrieval with product quantization compressing embeddings for ranking', "
        "'NeurIPS', 2025, ARRAY[0.80, 0.35, 0.05]),"
        "(4, 'LSM trees under write amplification', "
        "'storage engines log structured merge trees and write amplification tradeoffs', "
        "'VLDB', 2023, ARRAY[0.05, 0.95, 0.10]),"
        "(5, 'Graph pattern matching in RDBMS', "
        "'graph pattern matching compiled into relational plans with worst case optimal joins', "
        "'VLDB', 2025, ARRAY[0.10, 0.25, 0.95]),"
        "(6, 'Regular path queries on property graphs', "
        "'regular path queries automata evaluation over property graphs and traversal', "
        "'ICDE', 2018, ARRAY[0.05, 0.10, 0.90]);"
        "CREATE INDEX papers_embedding_hnsw ON papers USING hnsw (embedding);"
        "CREATE TABLE archived_papers (id INTEGER PRIMARY KEY, title TEXT, "
        "venue TEXT, archived_embedding VECTOR(3));"
        "INSERT INTO archived_papers VALUES "
        "(101, 'Sparse retrieval retrospective', 'SIGIR', ARRAY[0.93, 0.12, 0.04]),"
        "(102, 'Dense retrieval retrospective', 'NeurIPS', ARRAY[0.82, 0.34, 0.06]),"
        "(103, 'Storage engines retrospective', 'VLDB', ARRAY[0.08, 0.96, 0.08]),"
        "(104, 'Graph querying retrospective', 'ICDE', ARRAY[0.04, 0.12, 0.94]);"
        "CREATE INDEX archived_papers_embedding_hnsw ON archived_papers USING hnsw (archived_embedding);"
        "SELECT create_graph('citations')");
    const char *venues[] = {"SIGIR", "SIGIR", "NeurIPS", "VLDB", "VLDB", "ICDE"};
    char sql[512];
    for (int i = 0; i < 6; ++i) {
        int length = snprintf(sql, sizeof(sql),
            "SELECT * FROM cypher('citations', $$ CREATE "
            "(:Paper {paper_id: %d, venue: '%s'}) $$) AS (ignored agtype)", i + 1, venues[i]);
        REQUIRE(length > 0 && (size_t)length < sizeof(sql));
        example_execute(engine, sql);
    }
    const int cites[][2] = {{3, 1}, {3, 2}, {1, 2}, {5, 6}, {5, 4}};
    for (size_t i = 0; i < sizeof(cites) / sizeof(cites[0]); ++i) {
        int length = snprintf(sql, sizeof(sql),
            "SELECT * FROM cypher('citations', $$ MATCH (a:Paper {paper_id: %d}), "
            "(b:Paper {paper_id: %d}) CREATE (a)-[:CITES]->(b) $$) AS (ignored agtype)",
            cites[i][0], cites[i][1]);
        REQUIRE(length > 0 && (size_t)length < sizeof(sql));
        example_execute(engine, sql);
    }
}

int main(int argc, char **argv) {
    uqa_engine engine = example_open(example_arguments(argc, argv, 0));
    load(engine);
    uqa_udf_options options = NULL;
    uqa_udf function = NULL;
    CHECK(uqa_udf_options_create(&options));
    CHECK(uqa_udf_options_set_may_mutate_engine(options, 0));
    CHECK(uqa_udf_options_set_volatility(options, UQA_VOLATILITY_IMMUTABLE));
    CHECK(uqa_udf_register_scalar(engine, "recency_boost", options,
        recency_boost, NULL, NULL, &function));
    CHECK(uqa_udf_options_destroy(&options));

    const char *queries[] = {
        "SELECT id, title, year, _score FROM papers "
        "WHERE text_match(abstract, 'retrieval ranking') AND year >= 2020 "
        "ORDER BY _score DESC LIMIT 5",
        "SELECT id, title, year, _score FROM papers "
        "WHERE bayesian_match(abstract, 'retrieval ranking') AND year >= 2020 "
        "ORDER BY _score DESC LIMIT 5",
        "SELECT id, title, venue FROM papers WHERE knn_match(embedding, ARRAY[1.0, 0.0, 0.0], 3)",
        "SELECT id, title, _score FROM papers WHERE text_match(abstract, 'retrieval ranking') "
        "AND knn_match(embedding, ARRAY[1.0, 0.0, 0.0], 4) ORDER BY _score DESC LIMIT 4",
        "SELECT id, title, _score FROM papers WHERE pool_positive_evidence("
        "bayesian_match(abstract, 'retrieval ranking'), "
        "knn_match(embedding, ARRAY[1.0, 0.0, 0.0], 4)) ORDER BY _score DESC LIMIT 4",
        "SELECT pairs.left_doc_id AS live_id, pairs.right_doc_id AS archive_id, "
        "pairs._score AS score, p.title AS live_title, a.title AS archive_title "
        "FROM vector_similarity_join(papers, knn_match(embedding, ARRAY[1.0, 0.0, 0.0], 4), "
        "archived_papers, knn_match(archived_embedding, ARRAY[1.0, 0.0, 0.0], 4), 0.80) AS pairs "
        "JOIN papers AS p ON p.id = pairs.left_doc_id "
        "JOIN archived_papers AS a ON a.id = pairs.right_doc_id "
        "ORDER BY score DESC, live_id, archive_id LIMIT 8",
        "SELECT pairs.left_doc_id AS live_id, pairs.right_doc_id AS archive_id, "
        "pairs._score AS score, p.venue, a.title AS archive_title "
        "FROM hybrid_join(papers, venue IS NOT NULL "
        "AND knn_match(embedding, ARRAY[1.0, 0.0, 0.0], 6), "
        "archived_papers, venue IS NOT NULL "
        "AND knn_match(archived_embedding, ARRAY[1.0, 0.0, 0.0], 4)) AS pairs "
        "JOIN papers AS p ON p.id = pairs.left_doc_id "
        "JOIN archived_papers AS a ON a.id = pairs.right_doc_id "
        "ORDER BY score DESC, live_id, archive_id",
        "SELECT pairs.left_doc_id AS vertex_id, pairs.right_doc_id AS archive_id, a.title "
        "FROM cross_paradigm_join(papers, graph_pagerank('citations'), "
        "archived_papers, venue IS NOT NULL) AS pairs "
        "JOIN archived_papers AS a ON a.id = pairs.right_doc_id ORDER BY vertex_id, archive_id",
        "SELECT id, title, year, _score, _score * recency_boost(year) AS blended FROM papers "
        "WHERE text_match(abstract, 'retrieval ranking') ORDER BY blended DESC LIMIT 5"
    };
    int64_t seed = 0;
    for (size_t stage = 0; stage < sizeof(queries) / sizeof(queries[0]); ++stage) {
        uqa_result result = example_query(engine, queries[stage]);
        uint64_t count = example_rows(result);
        REQUIRE(count > 0);
        if (stage == 2) REQUIRE(example_integer(result, 0, 0) == 1);
        if (stage == 5 || stage == 6 || stage == 7) {
            for (uint64_t row = 0; row < count; ++row) {
                if (stage != 7) REQUIRE(example_integer(result, row, 0) < 100);
                REQUIRE(example_integer(result, row, 1) > 100);
            }
        }
        if (stage == 8) { seed = example_integer(result, 0, 0); REQUIRE(seed == 3); }
        CHECK(uqa_result_destroy(&result));
    }

    const char *graph_queries[] = {
        "SELECT p.id, p.title, p.venue, p.year, recency_boost(p.year) AS boost FROM papers AS p "
        "JOIN cypher('citations', $$ MATCH (:Paper {paper_id: %lld})-[:CITES]->(cited:Paper) "
        "RETURN cited.paper_id $$) AS cited(id int) ON p.id = cited.id "
        "WHERE p.venue <> 'ICDE' ORDER BY p.year DESC",
        "SELECT id, title, year FROM papers WHERE id IN (SELECT id FROM cypher('citations', $$ "
        "MATCH (:Paper {paper_id: %lld})-[:CITES]->()-[:CITES]->(r:Paper) RETURN r.paper_id $$) "
        "AS reached(id int)) ORDER BY year DESC",
        "SELECT id, title, year, _score * recency_boost(year) AS blended FROM papers "
        "WHERE text_match(abstract, 'retrieval ranking graph') AND year >= 2018 "
        "AND id IN (SELECT id FROM cypher('citations', $$ "
        "MATCH (:Paper {paper_id: %lld})-[:CITES*1..2]->(r:Paper) RETURN r.paper_id $$) "
        "AS reached(id int)) ORDER BY blended DESC LIMIT 5"
    };
    const int64_t cited[] = {1, 2};
    const int64_t reachable[] = {2};
    char sql[1024];
    for (size_t stage = 0; stage < 3; ++stage) {
        int length = snprintf(sql, sizeof(sql), graph_queries[stage], (long long)seed);
        REQUIRE(length > 0 && (size_t)length < sizeof(sql));
        uqa_result result = example_query(engine, sql);
        example_ids(result, stage == 1 ? reachable : cited, stage == 1 ? 1 : 2);
        CHECK(uqa_result_destroy(&result));
    }
    CHECK(uqa_udf_unregister(&function));
    CHECK(uqa_engine_close(&engine));
    puts("C unified search example passed");
    return EXIT_SUCCESS;
}
