"""Keep independently compiled managed/native private ABI discriminants aligned."""
from pathlib import Path
import re
import unittest

ROOT = Path(__file__).resolve().parents[1]


def snake(name):
    return re.sub(r"(?<!^)(?=[A-Z])", "_", name).lower()


def managed_protocol():
    source = (ROOT / "src/Native.Protocol.cs").read_text(encoding="utf-8")
    return {
        snake(domain): {
            snake(name).upper(): ("i32" if kind == "int" else "u32", int(value))
            for kind, name, value in re.findall(
                r"internal const (int|uint) (\w+) = (-?\d+);", body
            )
        }
        for domain, body in re.findall(
            r"internal static class (\w+)\s*\{([^{}]*)\}", source
        )
    }


class ProtocolContracts(unittest.TestCase):
    def test_managed_and_native_values_and_widths_match(self):
        source = (ROOT / "native/src/protocol.rs").read_text(encoding="utf-8")
        native = {
            domain: {
                name: (kind, int(value))
                for name, kind, value in re.findall(
                    r"pub const (\w+): (i32|u32) = (-?\d+);", body
                )
            }
            for domain, body in re.findall(
                r"pub\(crate\) mod (\w+)\s*\{([^{}]*)\}", source
            )
        }
        self.assertTrue(native, "Protocol inventory must not be empty")
        self.assertEqual(managed_protocol(), native)

    def test_public_discriminants_match_private_protocol(self):
        for path, public, domain in [
            ("McapReadCursor.cs", "McapCursorMode", "buffer_mode"),
            ("SansIo.cs", "McapReadEventKind", "engine_event"),
        ]:
            with self.subTest(public=public):
                source = (ROOT / "src" / path).read_text(encoding="utf-8")
                body = re.search(r"public enum " + public + r"\s*\{([^}]+)\}", source)[1]
                body = re.sub(r"//[^\n]*", "", body)
                values = {}
                value = 0
                for member in body.split(","):
                    member = member.strip()
                    if not member:
                        continue
                    if "=" in member:
                        member, explicit = member.split("=")
                        value = int(explicit.strip())
                    wire_name = {
                        "TopLevelRecords": "Linear",
                        "ExpandedRecordsWithoutMagic": "SansMagic",
                        "ExpandedRecords": "FlattenChunks",
                        "ChunkRecords": "Chunk",
                    }.get(member.strip(), member.strip()) if domain == "buffer_mode" else member.strip()
                    values[snake(wire_name).upper()] = ("u32", value)
                    value += 1
                self.assertEqual(managed_protocol()[domain], values)

    def test_equal_numbers_remain_distinct_status_domains(self):
        protocol = managed_protocol()
        self.assertEqual(protocol["reader_open_status"]["BUFFERED_SORT_REQUIRED"], ("i32", 3))
        self.assertEqual(protocol["batch_status"]["VISITOR_STOPPED"], ("i32", 3))
        self.assertEqual(protocol["callback_status"]["STOP"], ("i32", 1))
        self.assertEqual(protocol["status"]["END"], ("i32", 1))


if __name__ == "__main__":
    unittest.main()
