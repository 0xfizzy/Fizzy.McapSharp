"""Check the reviewed public-API inventory against the exact Cargo source used to build."""
import json
import os
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[1]
FILES = ["lib.rs", "write.rs", "read.rs", "records.rs", "sans_io/linear_reader.rs",
         "sans_io/indexed_reader.rs", "sans_io/summary_reader.rs", "sans_io/decompressor.rs", "tokio/linear_reader.rs", "storage.rs"]


def crate_source():
    source = ROOT / "native/vendor/mcap/src"
    if not source.is_dir():
        raise RuntimeError("Missing vendored mcap source")
    return source



def inventory(source):
    result = {}
    for file in FILES:
        owner = ""
        enum_owner = None
        public_owner = False
        source_text = (source / file).read_text(encoding="utf-8")
        public_types = set(re.findall(r"^pub (?:struct|enum|trait|type) (\w+)", source_text, re.MULTILINE))
        for line_number, line in enumerate(source_text.splitlines(), 1):
            if file == "sans_io/linear_reader.rs" and not line.startswith("pub ") and not public_owner:
                continue
            declaration = re.match(r"pub (?:struct|enum|trait|type) (\w+)", line)
            if declaration:
                owner = declaration[1]
                enum_owner = owner if line.startswith("pub enum") else None
                public_owner = True
                result[f"{file}::{owner}"] = "type"
            if enum_owner:
                variant = re.match(r"    ([A-Z]\w*)(?:\s*\{|\(|,|\s*=)", line)
                if variant:
                    result[f"{file}::{enum_owner}::{variant[1]}"] = "variant"
                if line == "}":
                    enum_owner = None
            impl = re.match(r"impl(?:<.*>)? (\w+)(?:<.*>)?(?:\s|$)", line)
            if impl:
                owner = impl[1]
                public_owner = not line.startswith("impl Iterator")
                if file == "storage.rs":
                    public_owner = owner in public_types
            if re.match(r"(?:struct|enum) ", line):
                public_owner = False
            member = re.match(r"    pub (?:async )?fn (\w+)", line)
            if member and public_owner:
                result[f"{file}::{owner}::{member[1]}"] = "method"
            function = re.match(r"pub (?:async )?fn (\w+)", line)
            if function:
                result[f"{file}::{function[1]}"] = "function"
            field = re.match(r"    pub (\w+):", line)
            if field and public_owner:
                result[f"{file}::{owner}::{field[1]}"] = "field"
            constant = re.match(r"( *)pub const (\w+)", line)
            if constant:
                scope = owner if file == "write.rs" else "op" if file == "records.rs" else ""
                result[f"{file}::{scope + '::' if scope else ''}{constant[2]}"] = "constant"
            if file == "sans_io/decompressor.rs":
                method = re.match(r"    fn (\w+)", line)
                if method:
                    result[f"{file}::Decompressor::{method[1]}"] = "method"
    return result


def main():
    actual = inventory(crate_source())
    manifest = json.loads((ROOT / "docs/api-coverage.json").read_text(encoding="utf-8"))
    official = json.loads((ROOT / "docs/upstream-api.json").read_text(encoding="utf-8"))["declarations"]
    if not set(official) <= set(actual):
        raise AssertionError("Local patch removed an upstream declaration")
    expected = {item["rust"]: item for item in manifest["items"]}
    if len(expected) != len(manifest["items"]):
        raise AssertionError("Duplicate reviewed API declaration")
    groups = set(manifest["groups"])
    mappings = set(manifest["mapping_categories"])
    if set(actual) != set(expected):
        raise AssertionError(f"Unreviewed API changes: missing={set(actual)-set(expected)}, stale={set(expected)-set(actual)}")
    for symbol, item in expected.items():
        origin = "upstream" if symbol in official else "local-extension"
        if item.get("origin") != origin or item["kind"] != actual[symbol]:
            raise AssertionError(f"Incorrect API origin/kind: {symbol}")
        if item.get("group") not in groups or item.get("mapping") not in mappings:
            raise AssertionError(f"Unknown API group/mapping: {symbol}")
        if (item["mapping"] == "local-extension") != (origin == "local-extension"):
            raise AssertionError(f"Incorrect extension mapping: {symbol}")
        if (item["group"] == "local-extensions") != (origin == "local-extension"):
            raise AssertionError(f"Incorrect extension group: {symbol}")
        if item["mapping"] in ("alternative", "not-exposed") and not item.get("reason"):
            raise AssertionError(f"Missing adaptation reason: {symbol}")
        for key in ["managed", "native", "test"]:
            if not item.get(key):
                raise AssertionError(f"Unmapped {symbol}: {key}")
        for file in item["files"]:
            if not (ROOT / file).is_file():
                raise AssertionError(f"Missing implementation/test file: {file}")
    print(f"Reviewed public API inventory: {len(actual)} declarations, mcap {manifest['version']}")


if __name__ == "__main__":
    main()
