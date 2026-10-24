/*
 * Unified Query Algebra
 *
 * Copyright (c) 2023-2026 Cognica, Inc.
 */

#include "example.h"

typedef struct neighbors {
    uint64_t count;
    int64_t ids[6];
    double scores[6];
} neighbors;

static neighbors knn(uqa_engine engine, int32_t count) {
    const float direction[] = {1, 0, 0, 0};
    uqa_value vector = NULL, limit = NULL;
    CHECK(uqa_value_create_vector_f32(direction, 4, &vector));
    CHECK(uqa_value_create_int32(count, &limit));
    uqa_value_view parameters[] = {vector, limit};
    uqa_result result = NULL;
    CHECK(uqa_result_create(&result));
    CHECK(uqa_engine_execute_params(engine,
        "SELECT id, _score FROM notes WHERE knn_match(embedding, $1, $2) "
        "ORDER BY _score DESC, id", parameters, 2, result));
    CHECK(uqa_value_destroy(&limit));
    CHECK(uqa_value_destroy(&vector));
    neighbors found = {0, {0}, {0}};
    found.count = example_rows(result);
    REQUIRE(found.count <= 6);
    for (uint64_t i = 0; i < found.count; ++i) {
        found.ids[i] = example_integer(result, i, 0);
        found.scores[i] = example_float(result, i, 1);
    }
    CHECK(uqa_result_destroy(&result));
    return found;
}

static void equal_neighbors(neighbors left, neighbors right) {
    REQUIRE(left.count == right.count);
    for (uint64_t i = 0; i < left.count; ++i) {
        REQUIRE(left.ids[i] == right.ids[i]);
        REQUIRE(left.scores[i] == right.scores[i]);
    }
}

static void replace_vector(uqa_engine engine) {
    const float direction[] = {1, 0, 0, 0};
    uqa_value vector = NULL;
    CHECK(uqa_value_create_vector_f32(direction, 4, &vector));
    uqa_value_view parameters[] = {vector};
    uqa_result result = NULL;
    CHECK(uqa_result_create(&result));
    CHECK(uqa_engine_execute_params(engine,
        "UPDATE notes SET embedding = $1 WHERE id = 6", parameters, 1, result));
    CHECK(uqa_result_destroy(&result));
    CHECK(uqa_value_destroy(&vector));
}

int main(int argc, char **argv) {
    example_database database = example_arguments(argc, argv, 0);
    uqa_engine engine = example_open(database);
    example_execute(engine,
        "CREATE TABLE notes (id INTEGER PRIMARY KEY, title TEXT, topic TEXT, embedding VECTOR(4));"
        "INSERT INTO notes VALUES "
        "(1, 'async runtimes', 'systems', ARRAY[0.95, 0.10, 0.05, 0.00]),"
        "(2, 'ownership and borrows', 'systems', ARRAY[0.90, 0.20, 0.00, 0.10]),"
        "(3, 'zero-copy parsing', 'systems', ARRAY[0.85, 0.05, 0.15, 0.05]),"
        "(4, 'sourdough starters', 'cooking', ARRAY[0.05, 0.95, 0.10, 0.00]),"
        "(5, 'knife skills', 'cooking', ARRAY[0.00, 0.90, 0.20, 0.05]),"
        "(6, 'fermentation basics', 'cooking', ARRAY[0.10, 0.85, 0.05, 0.15])");
    neighbors exact = knn(engine, 3);
    REQUIRE(exact.count == 3 && exact.ids[0] == 1 && exact.ids[1] == 3 && exact.ids[2] == 2);
    example_execute(engine, "CREATE INDEX notes_embedding_hnsw ON notes USING hnsw (embedding)");
    REQUIRE(knn(engine, 3).ids[0] == 1);
    example_execute(engine, "DROP INDEX notes_embedding_hnsw");
    example_execute(engine,
        "CREATE INDEX notes_embedding_ivf ON notes USING ivf (embedding) "
        "WITH (lists = 2, probes = 2, train_threshold = 4)");
    REQUIRE(knn(engine, 3).ids[0] == 1);
    example_execute(engine, "DROP INDEX notes_embedding_ivf");
    example_execute(engine,
        "CREATE INDEX notes_embedding_diskann ON notes USING diskann (embedding) "
        "WITH (max_degree = 4, search_list_size = 16, beam_width = 2)");
    equal_neighbors(knn(engine, 3), exact);
    uqa_result filtered = example_query(engine,
        "SELECT id, title, topic FROM notes "
        "WHERE knn_match(embedding, ARRAY[1.0, 0.0, 0.0, 0.0], 6) AND topic = 'cooking' "
        "ORDER BY _score DESC, id LIMIT 3");
    REQUIRE(example_rows(filtered) == 3);
    for (uint64_t i = 0; i < 3; ++i) example_text(filtered, i, 2, "cooking");
    CHECK(uqa_result_destroy(&filtered));
    example_execute(engine, "BEGIN");
    replace_vector(engine);
    neighbors private_rows = knn(engine, 3);
    REQUIRE(private_rows.ids[0] == 6 && private_rows.scores[0] == 1);
    example_execute(engine, "ROLLBACK");
    equal_neighbors(knn(engine, 3), exact);
    replace_vector(engine);
    neighbors committed = knn(engine, 6);
    const int64_t expected[] = {6, 1, 3, 2, 4, 5};
    REQUIRE(committed.count == 6 && committed.scores[0] == 1);
    for (size_t i = 0; i < 6; ++i) REQUIRE(committed.ids[i] == expected[i]);
    CHECK(uqa_engine_close(&engine));
    if (database.path != NULL) {
        engine = example_open(database);
        equal_neighbors(knn(engine, 6), committed);
        REQUIRE(example_one_integer(engine,
            "SELECT count(*) FROM pg_indexes WHERE indexname = 'notes_embedding_diskann' "
            "AND lower(indexdef) LIKE '%using diskann%'") == 1);
        CHECK(uqa_engine_close(&engine));
    }
    puts("C vector KNN example passed");
    return EXIT_SUCCESS;
}
