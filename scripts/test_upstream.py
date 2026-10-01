"""Compare a separately built, unpatched registry crate with the local patch."""
import os
from pathlib import Path
import subprocess
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[1]

def main():
    local = ROOT / ".tools/cargo/bin/cargo.exe"
    env = os.environ.copy()
    cargo = str(local) if local.exists() else "cargo"
    if local.exists():
        env["CARGO_HOME"] = str(ROOT / ".tools/cargo")
        env["RUSTUP_HOME"] = str(ROOT / ".tools/rustup")
    manifests = [ROOT / "tests/UpstreamReference/Cargo.toml", ROOT / "native/Cargo.toml"]
    reference = tomllib.loads(manifests[0].read_text())
    assert "patch" not in reference, "Reference must use unmodified registry source"
    locks = [tomllib.loads(p.with_name("Cargo.lock").read_text()) for p in manifests]
    native = {(p["name"], p["version"]) for p in locks[1]["package"]}
    for package in locks[0]["package"]:
        if package["name"] == reference["package"]["name"]:
            continue
        assert (package["name"], package["version"]) in native, "Reference dependencies differ"
        if package["name"] == "mcap":
            assert package["source"].startswith("registry+") and package["checksum"]
    outputs = []
    for manifest in manifests:
        command = [cargo, "run", "--release", "--locked", "--manifest-path", str(manifest),
                   "--target-dir", str(ROOT / "native/target/reference")]
        if manifest == manifests[1]: command += ["--example", "upstream_reference"]
        result = subprocess.run(command, cwd=ROOT, env=env, capture_output=True, timeout=180)
        if result.returncode:
            sys.stderr.buffer.write(result.stderr)
            raise RuntimeError("Reference comparison build/run failed")
        outputs.append(result.stdout)
    if outputs[0] != outputs[1]:
        folder = ROOT / "artifacts/upstream-comparison"
        folder.mkdir(parents=True, exist_ok=True)
        for name, output in zip(["official", "patched"], outputs):
            (folder / (name + ".txt")).write_bytes(output)
        raise AssertionError(f"Upstream behavior differs; inspect {folder}")
    print(f"Independent upstream comparison passed: {len(outputs[0].splitlines())} observations")

if __name__ == "__main__":
    main()
