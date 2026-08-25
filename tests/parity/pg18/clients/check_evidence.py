#!/usr/bin/env python3

import json
import pathlib
import sys


def main() -> None:
    if len(sys.argv) != 3:
        raise SystemExit("usage: check_evidence.py DRIVER OUTPUT")
    driver = sys.argv[1]
    output = pathlib.Path(sys.argv[2])
    expected_path = pathlib.Path(__file__).with_name("expected.json")
    expected = json.loads(expected_path.read_text(encoding="utf-8"))[driver]
    lines = [line for line in output.read_text(encoding="utf-8").splitlines() if line]
    if not lines:
        raise SystemExit(f"{driver} emitted no evidence")
    actual = json.loads(lines[-1])
    if actual != expected:
        raise SystemExit(
            f"{driver} evidence mismatch\nexpected: {expected!r}\nactual:   {actual!r}"
        )
    print(json.dumps(actual, sort_keys=True))


if __name__ == "__main__":
    main()
