from __future__ import annotations

import argparse
import copy
import os
import re
import sys
import tempfile
import zipfile
from pathlib import Path

MAX_INPUT = 256 * 1024 * 1024
MAX_EXPANDED = 512 * 1024 * 1024
MAX_ENTRIES = 32768
MAX_COMMON = 8 * 1024 * 1024
DEFAULT_ROOT = "ComplementaryDev"
SOURCE_LINE = b'float cloudHeightM = isnan(cloudHeight) ? 192.0 : cloudHeight; // iris returns "nan" if there are no clouds'
HORIZON_LINE = b'float cloudHeightM = (isnan(cloudHeight) || cloudHeight < 1.0) ? 192.0 : cloudHeight; // iris returns "nan" if there are no clouds; Horizon omits it as 0'


class ShaderError(Exception):
    pass


def safe_entry_name(name: str) -> bool:
    if not name or len(name) > 1024 or "\\" in name or "\x00" in name or name.startswith("/"):
        return False
    parts = name.rstrip("/").split("/")
    if any(part in ("", ".", "..") for part in parts):
        return False
    if re.match(r"^[A-Za-z]:", name):
        return False
    return True


def inspect_archive(archive: zipfile.ZipFile) -> tuple[zipfile.ZipInfo, str, int]:
    infos = archive.infolist()
    if not infos or len(infos) > MAX_ENTRIES:
        raise ShaderError("Archive is empty or contains too many entries")
    seen: set[str] = set()
    expanded = 0
    common: list[tuple[zipfile.ZipInfo, str]] = []
    for info in infos:
        name = info.filename
        if not safe_entry_name(name) or name in seen:
            raise ShaderError(f"Unsafe or duplicate ZIP path: {name!r}")
        seen.add(name)
        mode = info.external_attr >> 16
        if mode & 0o170000 == 0o120000:
            raise ShaderError(f"Symlink entry is not allowed: {name}")
        expanded += info.file_size
        if expanded > MAX_EXPANDED:
            raise ShaderError("Expanded ZIP exceeds 512 MiB")
        suffix = "shaders/lib/common.glsl"
        if not info.is_dir() and name.endswith(suffix):
            prefix = name[:-len(suffix)]
            common.append((info, prefix))
    if len(common) != 1:
        raise ShaderError("Could not identify exactly one shaders/lib/common.glsl")
    info, prefix = common[0]
    if prefix:
        for entry in infos:
            if not entry.filename.startswith(prefix):
                raise ShaderError("Shader ZIP mixes files inside and outside its pack folder")
    return info, prefix, expanded


def classify(data: bytes) -> str:
    source_count = data.count(SOURCE_LINE)
    horizon_count = data.count(HORIZON_LINE)
    if horizon_count == 1 and source_count == 0:
        return "horizon-compatible"
    if source_count == 1 and horizon_count == 0:
        return "convertible"
    if source_count > 1 or horizon_count > 1 or (source_count and horizon_count):
        return "ambiguous"
    return "unsupported"


def detect(path: Path) -> tuple[str, str]:
    if not path.is_file():
        raise ShaderError(f"Input file does not exist: {path}")
    if path.stat().st_size > MAX_INPUT:
        raise ShaderError("Input ZIP exceeds 256 MiB")
    try:
        with zipfile.ZipFile(path, "r") as archive:
            info, prefix, _ = inspect_archive(archive)
            if info.file_size > MAX_COMMON:
                raise ShaderError("common.glsl is unexpectedly large")
            data = archive.read(info)
    except (zipfile.BadZipFile, OSError, RuntimeError) as exc:
        raise ShaderError(f"Could not read ZIP: {exc}") from exc
    return classify(data), prefix


def convert(source: Path, destination: Path, pack_root: str | None = None) -> tuple[str, int]:
    if source.resolve() == destination.resolve():
        raise ShaderError("Input and output paths must be different")
    if not source.is_file():
        raise ShaderError(f"Input file does not exist: {source}")
    if source.stat().st_size > MAX_INPUT:
        raise ShaderError("Input ZIP exceeds 256 MiB")
    destination.parent.mkdir(parents=True, exist_ok=True)
    try:
        with zipfile.ZipFile(source, "r") as incoming:
            common_info, source_prefix, _ = inspect_archive(incoming)
            if common_info.file_size > MAX_COMMON:
                raise ShaderError("common.glsl is unexpectedly large")
            common_data = incoming.read(common_info)
            status = classify(common_data)
            if status in ("unsupported", "ambiguous"):
                raise ShaderError("No safe conversion rule matches this shader version; no output was written")
            converted_data = common_data.replace(SOURCE_LINE, HORIZON_LINE, 1) if status == "convertible" else common_data
            if pack_root is not None:
                root = pack_root
            elif source_prefix:
                root = source_prefix.rstrip("/").split("/")[-1]
            else:
                root = DEFAULT_ROOT
            if not root or root in (".", "..") or "/" in root or "\\" in root or any(ord(ch) < 32 for ch in root):
                raise ShaderError("Pack folder name is invalid")
            root_prefix = root + "/"
            fd, temp_name = tempfile.mkstemp(prefix=destination.name + ".", suffix=".tmp", dir=destination.parent)
            os.close(fd)
            temp_path = Path(temp_name)
            try:
                with zipfile.ZipFile(temp_path, "w") as outgoing:
                    outgoing.comment = incoming.comment
                    root_info = zipfile.ZipInfo(root_prefix)
                    root_info.create_system = 3
                    root_info.external_attr = (0o40755 << 16) | 0x10
                    root_info.compress_type = zipfile.ZIP_STORED
                    outgoing.writestr(root_info, b"")
                    entry_count = 1
                    for original in incoming.infolist():
                        name = original.filename
                        relative = name[len(source_prefix):] if source_prefix and name.startswith(source_prefix) else name
                        if source_prefix and not name.startswith(source_prefix):
                            raise ShaderError("Archive path escaped its shader pack root")
                        if source_prefix and not relative:
                            continue
                        mapped = root_prefix + relative
                        entry_count += 1
                        new_info = copy.copy(original)
                        new_info.filename = mapped
                        new_info.orig_filename = mapped
                        if original == common_info:
                            outgoing.writestr(new_info, converted_data)
                        else:
                            with incoming.open(original, "r") as reader, outgoing.open(new_info, "w") as writer:
                                copied = 0
                                while True:
                                    chunk = reader.read(65536)
                                    if not chunk:
                                        break
                                    copied += len(chunk)
                                    if copied > original.file_size:
                                        raise ShaderError(f"Unexpected expanded size: {original.filename}")
                                    writer.write(chunk)
                                if copied != original.file_size:
                                    raise ShaderError(f"Incomplete ZIP entry: {original.filename}")
                os.replace(temp_path, destination)
            finally:
                if temp_path.exists():
                    temp_path.unlink()
            return status, entry_count
    except ShaderError:
        raise
    except (zipfile.BadZipFile, OSError, RuntimeError, ValueError) as exc:
        raise ShaderError(f"Conversion failed: {exc}") from exc


def main() -> int:
    parser = argparse.ArgumentParser(prog="horizon_shader_converter")
    sub = parser.add_subparsers(dest="command", required=True)
    check_parser = sub.add_parser("check")
    check_parser.add_argument("input", type=Path)
    convert_parser = sub.add_parser("convert")
    convert_parser.add_argument("input", type=Path)
    convert_parser.add_argument("output", type=Path)
    convert_parser.add_argument("--root", dest="pack_root")
    args = parser.parse_args()
    try:
        if args.command == "check":
            status, prefix = detect(args.input)
            print(f"Compatibility: {status}")
            print(f"ZIP pack root: {prefix or '(archive root)'}")
            if status == "horizon-compatible":
                print("No conversion needed.")
                return 0
            if status == "convertible":
                print("A known Horizon compatibility conversion is available.")
                return 0
            print("This shader version is not covered by a verified conversion rule.")
            return 2
        status, entries = convert(args.input, args.output, args.pack_root)
        if status == "convertible":
            print("Converted: one verified common.glsl rule applied.")
        else:
            print("Already Horizon-compatible: shader source bytes were not changed.")
        print(f"Output: {args.output}")
        print(f"ZIP entries: {entries}")
        return 0
    except ShaderError as exc:
        print(f"Error: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
