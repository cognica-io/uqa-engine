#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Prepare a BEIR dataset for the beir_hybrid_eval example.

Downloads the named BEIR dataset, encodes the corpus and test queries
with a sentence-transformers model, and writes eval_corpus.jsonl /
eval_queries.jsonl into the output directory expected by
crates/uqa-engine/examples/beir_hybrid_eval.rs.

Usage:
    python3 tests/beir/encode_beir.py <output-dir> [dataset] [model]

Defaults: dataset=scifact, model=sentence-transformers/all-MiniLM-L6-v2.
Requires: sentence-transformers (pip install sentence-transformers).
"""

import io
import json
import sys
import urllib.request
import zipfile
from pathlib import Path

BEIR_BASE = "https://public.ukp.informatik.tu-darmstadt.de/thakur/BEIR/datasets"


def main() -> None:
    if len(sys.argv) < 2:
        raise SystemExit(__doc__)
    output_dir = Path(sys.argv[1])
    dataset = sys.argv[2] if len(sys.argv) > 2 else "scifact"
    model_name = (
        sys.argv[3]
        if len(sys.argv) > 3
        else "sentence-transformers/all-MiniLM-L6-v2"
    )
    output_dir.mkdir(parents=True, exist_ok=True)

    dataset_dir = output_dir / dataset
    if not dataset_dir.exists():
        print(f"downloading {dataset} ...")
        with urllib.request.urlopen(f"{BEIR_BASE}/{dataset}.zip") as response:
            archive = zipfile.ZipFile(io.BytesIO(response.read()))
        archive.extractall(output_dir)

    corpus = []
    with open(dataset_dir / "corpus.jsonl") as f:
        for line in f:
            row = json.loads(line)
            corpus.append((row["_id"], row.get("title", ""), row["text"]))

    qrels = {}
    with open(dataset_dir / "qrels" / "test.tsv") as f:
        next(f)
        for line in f:
            query_id, doc_id, score = line.strip().split("\t")
            qrels.setdefault(query_id, {})[doc_id] = float(score)

    queries = []
    with open(dataset_dir / "queries.jsonl") as f:
        for line in f:
            row = json.loads(line)
            if row["_id"] in qrels:  # test split only
                queries.append((row["_id"], row["text"]))
    print(f"corpus={len(corpus)} test_queries={len(queries)}")

    from sentence_transformers import SentenceTransformer

    model = SentenceTransformer(model_name)
    doc_texts = [(title + " " + text).strip() for _, title, text in corpus]
    doc_embeddings = model.encode(
        doc_texts, batch_size=128, normalize_embeddings=True
    )
    query_embeddings = model.encode(
        [text for _, text in queries], batch_size=128, normalize_embeddings=True
    )
    print("encoded", doc_embeddings.shape, query_embeddings.shape)

    # Remap document ids to sequential integers so the Rust example can
    # use them as engine doc ids regardless of the dataset's id scheme.
    doc_numbers = {doc_id: index + 1 for index, (doc_id, _, _) in enumerate(corpus)}

    with open(output_dir / "eval_corpus.jsonl", "w") as f:
        for (doc_id, title, text), embedding in zip(corpus, doc_embeddings):
            f.write(
                json.dumps(
                    {
                        "id": str(doc_numbers[doc_id]),
                        "body": (title + " " + text).strip(),
                        "embedding": [round(float(v), 6) for v in embedding],
                    }
                )
                + "\n"
            )

    with open(output_dir / "eval_queries.jsonl", "w") as f:
        for (query_id, text), embedding in zip(queries, query_embeddings):
            judgments = {
                str(doc_numbers[doc_id]): score
                for doc_id, score in qrels[query_id].items()
                if doc_id in doc_numbers
            }
            f.write(
                json.dumps(
                    {
                        "id": query_id,
                        "text": text,
                        "judgments": judgments,
                        "embedding": [round(float(v), 6) for v in embedding],
                    }
                )
                + "\n"
            )
    print(f"written to {output_dir}")


if __name__ == "__main__":
    main()
