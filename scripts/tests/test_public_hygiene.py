#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

from __future__ import annotations

import ast
import importlib.util
import pathlib
import subprocess
import tempfile
import unittest


SCRIPT = pathlib.Path(__file__).resolve().parents[1] / "check-public-repository-hygiene.py"
SPEC = importlib.util.spec_from_file_location("uqa_public_hygiene", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
HYGIENE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(HYGIENE)


class PublicHygieneTest(unittest.TestCase):
    def test_rust_fields_methods_and_lifetimes_are_code(self):
        left, right, method = (name + "." + field for name, field in
                               [("left", "internal"), ("right", "internal"), ("engine", "local")])
        source = f"fn value<'a>(left: &'a Item) {{ {left} == {right}; {method}(); }}"
        self.assertEqual(HYGIENE.violating_lines("source.rs", source), [])

    def test_hosts_in_strings_raw_strings_and_nested_comments_are_rejected(self):
        host = "node." + "internal"
        for source in [
            f'let x = "{host}";', f'let x = b"{host}";',
            f'let x = r##"quoted \" data {host}"##;',
            f'let x = br#"{host}"#;', f'let x = cr#"{host}"#;',
            f'// {host}', f'/* outer /* nested */ {host} */',
            f'let c = \'"\'; let x = "{host}";',
            f'let c = \'/\'; /* {host} */',
            f'let x = "escaped \\\" {host}";',
            f'let x = "{host}', f'/* {host}',
        ]:
            with self.subTest(source=source):
                self.assertEqual(HYGIENE.violating_lines("source.rs", source), [1])
        self.assertEqual(HYGIENE.violating_lines("README.md", host), [1])

    def test_other_policies_apply_in_every_rust_context(self):
        for text in [
            "/" + "Users/example/project/", "ghp_" + "a" * 24,
            "C" + "puBackend", "https://" + "192.168.1.3/api",
        ]:
            for source in [f'// {text}', f'let x = "{text}";']:
                with self.subTest(source=source):
                    self.assertEqual(HYGIENE.violating_lines("source.rs", source), [1])
        self.assertEqual(HYGIENE.violating_lines("source.rs", "C" + "puBackend"), [1])

    def test_line_numbers_survive_multiline_literals_comments_and_character_tokens(self):
        host = "node." + "local"
        field = "value." + "internal"
        source = f'''let c = '"';
let x = r#"first
{host}
"#;
/* outer
 /* nested */ {host}
*/
let x = {field};
'''
        self.assertEqual(HYGIENE.violating_lines("source.rs", source), [3, 6])

    def test_repository_covers_tracked_untracked_ignored_and_generated_inputs(self):
        host = "node." + "internal"
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            subprocess.run(["git", "init", "-q", str(root)], check=True)
            (root / ".gitignore").write_text("ignored.txt\n")
            (root / "source.rs").write_text("let ok = left." + "internal;\n")
            (root / "tracked.md").write_text("text\u2028" + host + "\n")
            subprocess.run(["git", "add", "."], cwd=root, check=True)
            (root / "untracked.md").write_text(host + "\n")
            (root / "ignored.txt").write_text(host + "\n")
            evidence = root / "benchmarks/nori/evidence/report.json"
            evidence.parent.mkdir(parents=True)
            evidence.write_bytes(b"\0binary evidence")
            (root / "binary.bin").write_bytes(b"\0" + host.encode())
            (root / "external.rs").symlink_to(host)
            result = HYGIENE.check_repository(root)
            self.assertEqual(len(result), 4, result)
            self.assertIn("tracked.md:1:text\u2028" + host, result)
            self.assertTrue(any(line.startswith("untracked.md:1:") for line in result))
            self.assertTrue(any("generated benchmark reports" in line for line in result))
            self.assertIn("external.rs:1:" + host, result)

    def test_scanner_accepts_python_3_8_grammar(self):
        ast.parse(SCRIPT.read_text(), feature_version=8)


if __name__ == "__main__":
    unittest.main()
