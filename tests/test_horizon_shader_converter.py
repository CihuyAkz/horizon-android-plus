import sys
import tempfile
import unittest
import zipfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "tools"))
import horizon_shader_converter as converter


class HorizonShaderConverterTests(unittest.TestCase):
    def make_archive(self, path: Path, line: bytes, wrapped: bool = False) -> None:
        prefix = "ComplementaryDev/" if wrapped else ""
        with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as archive:
            if wrapped:
                archive.writestr(prefix, b"")
            archive.writestr(prefix + "shaders/", b"")
            archive.writestr(prefix + "shaders/lib/", b"")
            archive.writestr(prefix + "shaders/lib/common.glsl", b"void setup() {\n    " + line + b"\n}\n")
            archive.writestr(prefix + "shaders/final.fsh", b"unchanged shader source\n")
            archive.writestr(prefix + "license.txt", b"unchanged license\n")

    def test_convert_changes_only_the_verified_common_line(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            source = Path(tmp) / "input.zip"
            output = Path(tmp) / "output.zip"
            self.make_archive(source, converter.SOURCE_LINE)
            self.assertEqual(converter.detect(source)[0], "convertible")
            status, entries = converter.convert(source, output)
            self.assertEqual(status, "convertible")
            self.assertEqual(entries, 6)
            with zipfile.ZipFile(output) as archive:
                self.assertIn("ComplementaryDev/shaders/lib/common.glsl", archive.namelist())
                common = archive.read("ComplementaryDev/shaders/lib/common.glsl")
                self.assertIn(converter.HORIZON_LINE, common)
                self.assertNotIn(converter.SOURCE_LINE, common)
                self.assertEqual(archive.read("ComplementaryDev/shaders/final.fsh"), b"unchanged shader source\n")
                self.assertEqual(archive.read("ComplementaryDev/license.txt"), b"unchanged license\n")

    def test_already_converted_archive_is_detected_without_shader_changes(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            source = Path(tmp) / "converted.zip"
            output = Path(tmp) / "copied.zip"
            self.make_archive(source, converter.HORIZON_LINE, wrapped=True)
            self.assertEqual(converter.detect(source)[0], "horizon-compatible")
            status, _ = converter.convert(source, output)
            self.assertEqual(status, "horizon-compatible")
            with zipfile.ZipFile(output) as archive:
                self.assertIn("ComplementaryDev/shaders/lib/common.glsl", archive.namelist())
                self.assertEqual(
                    archive.read("ComplementaryDev/shaders/lib/common.glsl"),
                    zipfile.ZipFile(source).read("ComplementaryDev/shaders/lib/common.glsl"),
                )

    def test_unknown_shader_is_not_rewritten(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            source = Path(tmp) / "unknown.zip"
            output = Path(tmp) / "must-not-exist.zip"
            self.make_archive(source, b"float cloudHeightM = cloudHeight;")
            self.assertEqual(converter.detect(source)[0], "unsupported")
            with self.assertRaises(converter.ShaderError):
                converter.convert(source, output)
            self.assertFalse(output.exists())


if __name__ == "__main__":
    unittest.main()
