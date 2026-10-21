#!/usr/bin/env python3
"""Verify the pinned historical lowercase delta against independently downloaded UCD files."""
from pathlib import Path
import hashlib
import re
import sys

HASHES = {'16.0.0-UnicodeData.txt': 'ff58e5823bd095166564a006e47d111130813dcf8bf234ef79fa51a870edb48f', '16.0.0-SpecialCasing.txt': '8d5de354eef79f2395a54c9c7dcebbaf3d30fc962d0f85611ea97aa973a0c451', '16.0.0-PropList.txt': '53d614508e2a0b2305a8aa21cd60d993de9326cdf65993660dfcce4503548583', '17.0.0-UnicodeData.txt': '2e1efc1dcb59c575eedf5ccae60f95229f706ee6d031835247d843c11d96470c', '17.0.0-SpecialCasing.txt': 'efc25faf19de21b92c1194c111c932e03d2a5eaf18194e33f1156e96de4c9588', '17.0.0-PropList.txt': '130dcddcaadaf071008bdfce1e7743e04fdfbc910886f017d9f9ac931d8c64dd'}

def load(root, version, name):
    key = f"{version}-{name}"
    data = (root / key).read_bytes()
    if hashlib.sha256(data).hexdigest() != HASHES[key]:
        raise SystemExit(f"hash mismatch: {key}")
    return data.decode().splitlines()

def lowercase(root, version):
    result = {}
    for line in load(root, version, "UnicodeData.txt"):
        fields = line.split(";")
        if len(fields) != 15:
            raise SystemExit(f"invalid UnicodeData record in {version}")
        if fields[13]:
            result[int(fields[0], 16)] = (int(fields[13], 16),)
    for line in load(root, version, "SpecialCasing.txt"):
        fields = [field.strip() for field in line.split("#")[0].split(";")]
        if len(fields) >= 5 and not fields[4]:
            result[int(fields[0], 16)] = tuple(int(c, 16) for c in fields[1].split())
    return result

def whitespace(root, version):
    result = set()
    for line in load(root, version, "PropList.txt"):
        fields = [field.strip() for field in line.split("#")[0].split(";")]
        if len(fields) == 2 and fields[1] == "White_Space":
            bounds = [int(c, 16) for c in fields[0].split("..")]
            result.update(range(bounds[0], bounds[-1] + 1))
    return result

def main():
    root = Path(sys.argv[1])
    old, new = [lowercase(root, v) for v in ("16.0.0", "17.0.0")]
    delta = [(c, new.get(c, (c,))) for c in sorted(old.keys() | new.keys())
             if old.get(c, (c,)) != new.get(c, (c,))]
    if len(delta) != 28 or any(len(mapping) != 1 for _, mapping in delta):
        raise SystemExit("unexpected full lowercase delta")
    source = (Path(__file__).resolve().parents[1] /
              "crates/uqa-analysis/src/unicode/unicode_17.rs").read_text()
    actual = [tuple(int(c, 16) for c in pair) for pair in
              re.findall(r"\('\\u\{([0-9a-f]+)\}', '\\u\{([0-9a-f]+)\}'\)", source)]
    if actual != [(c, mapping[0]) for c, mapping in delta]:
        raise SystemExit("committed lowercase table differs from the verified UCD inputs")
    if whitespace(root, "16.0.0") != whitespace(root, "17.0.0"):
        raise SystemExit("retained whitespace sets differ")
    print("28 full lowercase deltas and identical whitespace sets verified")

if __name__ == "__main__":
    main()
