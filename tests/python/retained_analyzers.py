#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Check retained catalog recovery across actual enabled/disabled wheel processes."""

import argparse
import hashlib
import json
from pathlib import Path

import uqa


def digest(path):
    result = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(65536), b""):
            result.update(chunk)
    return result.hexdigest()


def query(engine, language, query_text):
    assert engine.sql("SELECT id FROM docs WHERE text_match(body, $1)", [query_text]).rows == [{"id": 1}]
    return engine.sql("SELECT analysis FROM analyze_text($1, $2)", [f"retained_{language}", query_text]).rows


def create(directory):
    directory.mkdir(parents=True, exist_ok=True)
    records = []
    for language, text, query_text in (("nori", "한국 경제", "한국"), ("kuromoji", "東京大学", "東京")):
        path = directory / f"{language}.db"
        if path.exists():
            raise RuntimeError(f"refusing to replace an existing test database: {path}")
        engine = uqa.open(path)
        try:
            engine.sql("SELECT * FROM create_analyzer($1, $2)", [f"retained_{language}", json.dumps({"tokenizer": {"type": f"{language}_tokenizer"}})])
            engine.sql("CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT)")
            engine.sql("CREATE INDEX docs_fts ON docs USING gin (body)")
            engine.sql("SELECT * FROM set_table_analyzer('docs', 'body', $1)", [f"retained_{language}"])
            engine.sql("INSERT INTO docs VALUES (1, $1)", [text])
            rows = query(engine, language, query_text)
        finally:
            engine.close()
        records.append({"language": language, "query": query_text, "rows": rows, "database_sha256": digest(path)})
    (directory / "contract.json").write_text(json.dumps(records, ensure_ascii=False) + "\n", encoding="utf-8")


def check(directory, reject):
    for record in json.loads((directory / "contract.json").read_text(encoding="utf-8")):
        language = record["language"]
        path = directory / f"{language}.db"
        assert digest(path) == record["database_sha256"], "database changed before verification"
        if reject:
            try:
                engine = uqa.open(path)
            except RuntimeError as error:
                assert "unknown variant" in str(error) and f"{language}_tokenizer" in str(error), str(error)
            else:
                engine.close()
                raise AssertionError(f"disabled {language} unexpectedly opened its retained catalog")
            assert digest(path) == record["database_sha256"], "failed restoration modified the database"
        else:
            engine = uqa.open(path)
            try:
                assert query(engine, language, record["query"]) == record["rows"], "retained revision changed"
            finally:
                engine.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("create", "reject", "verify"))
    parser.add_argument("directory", type=Path)
    args = parser.parse_args()
    if args.mode == "create":
        create(args.directory)
    else:
        check(args.directory, args.mode == "reject")
    print(f"Retained Nori/Kuromoji catalogs: {args.mode} passed")
