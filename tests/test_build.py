"""Regression checks for artifact provenance and Linux compatibility gates."""
import hashlib
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
import build


class ArtifactChecks(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.identity = {"commit": "test-commit", "source_sha256": "test-source"}

    def tearDown(self):
        self.temp.cleanup()

    def stage(self):
        # Minimal headers are unit-test inputs only, never publishable artifacts.
        for rid, (target, filename) in build.TARGETS.items():
            folder = self.root / "artifacts/native" / rid
            folder.mkdir(parents=True)
            data = bytearray(128)
            if rid == "win-x64":
                data[:2] = b"MZ"
                data[60:64] = (64).to_bytes(4, "little")
                data[64:70] = b"PE\0\0\x64\x86"
            else:
                data[:6] = b"\x7fELF\x02\x01"
                data[18:20] = (62 if rid == "linux-x64" else 183).to_bytes(2, "little")
            (folder / filename).write_bytes(data)
            manifest = dict(self.identity, rid=rid, target=target, sha256=hashlib.sha256(data).hexdigest())
            (folder / "manifest.json").write_text(json.dumps(manifest), encoding="utf-8")

    def verify(self):
        with patch.object(build, "ROOT", self.root), patch.object(build, "source_identity", return_value=self.identity):
            build.verify_assets()

    def test_matching_assets(self):
        self.stage()
        self.verify()

    def test_missing_platform(self):
        self.stage()
        (self.root / "artifacts/native/linux-arm64/manifest.json").unlink()
        with self.assertRaisesRegex(RuntimeError, "Missing linux-arm64"):
            self.verify()

    def test_stale_source_or_commit(self):
        self.stage()
        for key in self.identity:
            with self.subTest(key=key):
                original = self.identity[key]
                self.identity[key] = "different"
                with self.assertRaisesRegex(RuntimeError, "Stale or mismatched"):
                    self.verify()
                self.identity[key] = original

    def test_modified_binary(self):
        self.stage()
        path = self.root / "artifacts/native/linux-x64/libfizzy_mcap_native.so"
        path.write_bytes(path.read_bytes() + b"changed")
        with self.assertRaisesRegex(RuntimeError, "Stale or mismatched"):
            self.verify()

    def test_wrong_architecture(self):
        self.stage()
        path = self.root / "artifacts/native/linux-x64/libfizzy_mcap_native.so"
        with self.assertRaisesRegex(RuntimeError, "Wrong ELF architecture"):
            build.check_binary(path, "linux-arm64")

    def test_glibc_baseline(self):
        with patch.object(build, "output", side_effect=["GLIBC_2.17 GLIBC_2.35", "(NEEDED) Shared library: [libc.so.6]"]):
            build.audit_linux(Path("test.so"))
        with patch.object(build, "output", return_value="GLIBC_2.36"):
            with self.assertRaisesRegex(RuntimeError, "GLIBC baseline"):
                build.audit_linux(Path("test.so"))

    def test_external_compression_and_search_paths_rejected(self):
        for dynamic in ["(NEEDED) Shared library: [libzstd.so.1]", "(NEEDED) Shared library: [libc.so.6] (RUNPATH)"]:
            with self.subTest(dynamic=dynamic), patch.object(build, "output", side_effect=["GLIBC_2.35", dynamic]):
                with self.assertRaisesRegex(RuntimeError, "Unexpected ELF"):
                    build.audit_linux(Path("test.so"))

    def test_host_matrix(self):
        for system, machine, libc, expected in [
            ("Windows", "AMD64", "", "win-x64"),
            ("Linux", "x86_64", "glibc", "linux-x64"),
            ("Linux", "aarch64", "glibc", "linux-arm64"),
            ("Windows", "ARM64", "", None),
            ("Linux", "aarch64", "musl", None),
            ("Darwin", "arm64", "", None),
        ]:
            with self.subTest(system=system, machine=machine, libc=libc), \
                 patch.object(build.platform, "system", return_value=system), \
                 patch.object(build.platform, "machine", return_value=machine), \
                 patch.object(build.platform, "libc_ver", return_value=(libc, "")):
                if expected:
                    self.assertEqual(expected, build.host_rid())
                else:
                    with self.assertRaises(RuntimeError):
                        build.host_rid()


if __name__ == "__main__":
    unittest.main()
