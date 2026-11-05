#!/usr/bin/env python3
"""Offline indexer for the Workers + R2 edition of sspks.

Scans `packages/*.spk`, parses the DSM 7 `INFO` metadata (a faithful port of
SSpkS\\Package\\Package), extracts package icons/screenshots and writes:

    dist/index.json          the complete package metadata for the Worker
    dist/index.report.json   per-file result report (ok / skipped / error)
    dist/icons/*.png         icons and screenshots referenced by index.json

Nothing is uploaded unless `--upload` is passed (R2 S3 API, see tools/r2.py).

Never supported here: official keytype-3 encrypted SPKs.  They need libsodium
and the PHP implementation; the indexer detects them and reports them as
skipped so they never silently disappear from the index.
"""
from __future__ import annotations

import argparse
import base64
import binascii
import hashlib
import json
import os
import re
import shlex
import shutil
import subprocess
import sys
import tarfile
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ASSETS = ROOT / "assets"
DEFAULT_ICON_72 = "icons/_default_72.png"
DEFAULT_ICON_120 = "icons/_default_120.png"

MAX_INFO_BYTES = 4 * 1024 * 1024
MAX_IMAGE_BYTES = 20 * 1024 * 1024
SCHEMA_VERSION = 1
BUNDLE_SCHEMA_VERSION = 2
ENCRYPTED_MARKER = b"\xad\xbe\xef"  # bytes 1..3 of an official keytype-3 archive

REQUIRED_INFO_FIELDS = ("package", "version", "os_min_ver", "description", "arch", "maintainer")
FORBIDDEN_IN_PACKAGE_NAME = re.compile(r"[:/><|=]")
INFO_LINE = re.compile(r"^\s*([A-Za-z][A-Za-z0-9_]*)\s*=(.*)$")
ARCH_LIST = re.compile(r"^[a-z0-9_]+(?:\s+[a-z0-9_]+)*$", re.IGNORECASE)
DSM_VERSION = re.compile(r"^(\d+)\.(\d+)(?:\.(\d+))?-(\d+)$")
PACKAGE_VERSION = re.compile(r"^\d+(?:[._-]\d+)+$")
LOCALIZED_KEY = re.compile(r"^(displayname|description)_([a-z]{3})$")
SCREEN_NAME = re.compile(r"^screen_(\d{1,2})\.png$")


class SpkError(Exception):
    """A single package could not be indexed; the rest of the scan continues."""


# ---------------------------------------------------------------------------
# INFO parsing — port of Package::parseInfoFile / decodeInfoValue
# ---------------------------------------------------------------------------

def parse_info(content: bytes) -> dict[str, str]:
    if len(content) > MAX_INFO_BYTES:
        raise SpkError("INFO exceeds the size limit")
    if content[:3] == b"\xef\xbb\xbf":
        content = content[3:]
    if b"\x00" in content:
        raise SpkError("INFO must use valid UTF-8 encoding")
    try:
        text = content.decode("utf-8")
    except UnicodeDecodeError as error:
        raise SpkError("INFO must use valid UTF-8 encoding") from error

    lines = re.split(r"\r\n|\r|\n", text)
    metadata: dict[str, str] = {}
    index = 0
    line_count = len(lines)
    while index < line_count:
        line = lines[index]
        trimmed = line.strip()
        index += 1
        if trimmed == "" or trimmed[0] in "#;":
            continue
        match = INFO_LINE.match(line)
        if match is None:
            raise SpkError(f"invalid INFO syntax on line {index}")
        key = match.group(1)
        value = match.group(2).strip()
        value_line = index
        while not _is_complete_quoted_value(value.rstrip()) and index < line_count:
            value += "\n" + lines[index]
            index += 1
        metadata[key] = _decode_info_value(value.rstrip(), value_line)

    if not metadata:
        raise SpkError("invalid INFO file")
    return metadata


def _is_complete_quoted_value(value: str) -> bool:
    if value == "" or value[0] not in "\"'":
        return True
    quote = value[0]
    for position in range(len(value) - 1, 0, -1):
        if value[position] != quote:
            continue
        if quote == "'":
            return position == len(value) - 1
        backslashes = 0
        offset = position - 1
        while offset >= 0 and value[offset] == "\\":
            backslashes += 1
            offset -= 1
        return position == len(value) - 1 and backslashes % 2 == 0
    return False


def _decode_info_value(value: str, line_number: int) -> str:
    if value == "" or value[0] not in "\"'":
        return value
    quote = value[0]
    if len(value) < 2 or value[-1] != quote:
        raise SpkError(f"unclosed quote in INFO on line {line_number}")
    inner = value[1:-1]
    if quote == "'":
        return inner
    decoded: list[str] = []
    position = 0
    while position < len(inner):
        character = inner[position]
        if character == "\\" and position + 1 < len(inner) and inner[position + 1] in "\\\"$`":
            decoded.append(inner[position + 1])
            position += 2
            continue
        decoded.append(character)
        position += 1
    return "".join(decoded)


def parse_bool(value) -> bool:
    if isinstance(value, bool):
        return value
    if isinstance(value, (int, float)):
        return value == 1
    if not isinstance(value, str):
        return False
    return value.strip().lower() in ("true", "yes", "1")


# ---------------------------------------------------------------------------
# Version helpers — port of Package::isValidDsmVersion and friends
# ---------------------------------------------------------------------------

def is_unsigned_int32(value: str) -> bool:
    normalised = value.lstrip("0")
    if normalised == "":
        return True
    return len(normalised) < 10 or (len(normalised) == 10 and normalised <= "2147483647")


def is_valid_dsm_version(version: str) -> bool:
    match = DSM_VERSION.match(version)
    if match is None:
        return False
    return all(part == "" or is_unsigned_int32(part) for part in match.groups() if part is not None)


def is_valid_package_version(version: str) -> bool:
    if PACKAGE_VERSION.match(version) is None:
        return False
    return all(is_unsigned_int32(part) for part in re.split(r"[._-]", version))


def compare_dsm_versions(left: str, right: str) -> int:
    def normalise(version: str) -> tuple[int, int, int]:
        if not is_valid_dsm_version(version):
            raise ValueError(f"invalid DSM version: {version}")
        release, build = version.split("-", 1)
        parts = release.split(".")
        return int(parts[0]), int(parts[1]), int(build)

    a = normalise(left)
    b = normalise(right)
    return (a > b) - (a < b)


def normalise_list(value: str, lowercase: bool = False) -> list[str]:
    items = [item for item in re.split(r"\s+", value.strip()) if item]
    if lowercase:
        items = [item.lower() for item in items]
    seen: dict[str, None] = {}
    for item in items:
        seen.setdefault(item, None)
    return list(seen)


# ---------------------------------------------------------------------------
# SPK archive reading
# ---------------------------------------------------------------------------

def _normalise_member_name(name: str) -> str:
    name = name.replace("\\", "/")
    while name.startswith("./"):
        name = name[2:]
    return name.lstrip("/")


def read_spk(path: Path) -> dict:
    with path.open("rb") as handle:
        head = handle.read(4)
    if len(head) == 4 and head[1:4] == ENCRYPTED_MARKER:
        raise SpkError("官方加密 SPK（keytype 3）暂不支持：请改用 PHP 版索引，或移除该文件")

    collected: dict = {"info": None, "icons": {}, "screens": {}, "wizard": False, "privilege": None}
    try:
        with tarfile.open(path, mode="r:*") as archive:
            for member in archive:
                name = _normalise_member_name(member.name)
                if not name or ".." in Path(name).parts:
                    continue
                if not member.isfile():
                    if name.rstrip("/") == "WIZARD_UIFILES":
                        collected["wizard"] = True
                    continue
                if name == "INFO":
                    if member.size > MAX_INFO_BYTES:
                        raise SpkError("INFO exceeds the size limit")
                    handle = archive.extractfile(member)
                    collected["info"] = handle.read() if handle is not None else None
                elif name in ("PACKAGE_ICON.PNG", "PACKAGE_ICON_256.PNG"):
                    if member.size > MAX_IMAGE_BYTES:
                        continue
                    key = "120" if name == "PACKAGE_ICON_256.PNG" else "72"
                    handle = archive.extractfile(member)
                    if handle is not None:
                        collected["icons"][key] = handle.read()
                elif name == "conf/privilege":
                    if member.size <= 1024 * 1024:
                        handle = archive.extractfile(member)
                        collected["privilege"] = handle.read() if handle is not None else None
                else:
                    screen = SCREEN_NAME.match(name)
                    if screen is not None:
                        number = int(screen.group(1))
                        if 1 <= number <= 20 and member.size <= MAX_IMAGE_BYTES:
                            handle = archive.extractfile(member)
                            if handle is not None:
                                collected["screens"][number] = handle.read()
    except tarfile.TarError as error:
        raise SpkError(f"无法解析归档: {error}") from error

    if collected["info"] is None:
        raise SpkError("归档中缺少 INFO")
    return collected


# ---------------------------------------------------------------------------
# Record building
# ---------------------------------------------------------------------------

def _runs_as_root(info: dict[str, str], privilege_bytes: bytes | None) -> bool:
    for key, value in info.items():
        if not key.startswith("description") and not key.lower().startswith("description"):
            continue
        text = str(value).lower()
        if "root权限" in text or "root privileges" in text:
            return True
    if privilege_bytes is None:
        return False
    if privilege_bytes[:3] == b"\xef\xbb\xbf":
        privilege_bytes = privilege_bytes[3:]
    try:
        privilege = json.loads(privilege_bytes.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError):
        return False
    return isinstance(privilege, dict) and str(privilege.get("defaults", {}).get("run-as", "")).strip().lower() == "root"


def _md5(path: Path) -> str:
    digest = hashlib.md5()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def build_record(path: Path, stem: str, write_icon) -> dict:
    raw = read_spk(path)
    info = parse_info(raw["info"])

    for field in REQUIRED_INFO_FIELDS:
        if not str(info.get(field, "")).strip():
            raise SpkError(f"缺少必需 INFO 字段: {field}")
    package = info["package"]
    if FORBIDDEN_IN_PACKAGE_NAME.search(package):
        raise SpkError("package 字段包含非法字符")
    if not is_valid_package_version(info["version"]):
        raise SpkError("version 字段格式非法")
    if not ARCH_LIST.match(info["arch"].strip()):
        raise SpkError("arch 字段格式非法")
    if not is_valid_dsm_version(info["os_min_ver"].strip()):
        raise SpkError("os_min_ver 字段格式非法")
    if compare_dsm_versions(info["os_min_ver"].strip(), "7.0-40000") < 0:
        raise SpkError("os_min_ver 不能低于 7.0-40000")
    os_max_ver = info.get("os_max_ver", "").strip()
    if os_max_ver != "":
        if not is_valid_dsm_version(os_max_ver):
            raise SpkError("os_max_ver 字段格式非法")
        if compare_dsm_versions(os_max_ver, info["os_min_ver"].strip()) < 0:
            raise SpkError("os_max_ver 不能低于 os_min_ver")

    displayname = info.get("displayname") or package
    record: dict = {
        "file": path.name,
        "package": package,
        "version": info["version"],
        "displayname": displayname,
        "description": info["description"],
        "os_min_ver": info["os_min_ver"].strip(),
        "os_max_ver": os_max_ver,
        "arch": normalise_list(info["arch"], lowercase=True),
        "exclude_arch": normalise_list(info.get("exclude_arch", ""), lowercase=True),
        "model": normalise_list(info.get("model", ""), lowercase=True),
        "exclude_model": normalise_list(info.get("exclude_model", ""), lowercase=True),
        "beta": parse_bool(info.get("beta", "")),
        "official": False,
        "run_as_root": _runs_as_root(info, raw["privilege"]),
        "silent_install": parse_bool(info.get("silent_install", "")),
        "silent_uninstall": parse_bool(info.get("silent_uninstall", "")),
        "silent_upgrade": parse_bool(info.get("silent_upgrade", "")),
        "auto_upgrade_from": info.get("auto_upgrade_from", ""),
        "install_dep_packages": info.get("install_dep_packages", ""),
        "install_dep_services": info.get("start_dep_services", ""),
        "install_conflict_packages": info.get("install_conflict_packages", ""),
        "maintainer": info.get("maintainer", ""),
        "maintainer_url": info.get("maintainer_url", ""),
        "distributor": info.get("distributor", ""),
        "distributor_url": info.get("distributor_url", ""),
        "support_url": info.get("support_url", ""),
        "changelog": info.get("changelog", ""),
        "filesize": path.stat().st_size,
        "md5": _md5(path),
    }

    # `hasWizardDir()` default: packages without a wizard support quick
    # install/start/upgrade flags.
    default_q = not raw["wizard"]
    for flag in ("qinst", "qstart", "qupgrade"):
        record[flag] = parse_bool(info[flag]) if flag in info else default_q

    for key, value in info.items():
        if LOCALIZED_KEY.match(key):
            record[key] = value

    thumbnail: list[str] = []
    for size in ("72", "120"):
        image = raw["icons"].get(size)
        if image is None:
            embedded = info.get("package_icon_256" if size == "120" else "package_icon")
            if embedded:
                try:
                    decoded = base64.b64decode(embedded, validate=True)
                except (ValueError, binascii.Error):
                    decoded = b""
                if 0 < len(decoded) <= 5 * 1024 * 1024:
                    image = decoded
        if image is None:
            thumbnail.append(DEFAULT_ICON_72 if size == "72" else DEFAULT_ICON_120)
        else:
            thumbnail.append(write_icon(f"{stem}_thumb_{size}.png", image))
    record["thumbnail"] = thumbnail

    snapshots: list[str] = []
    for number in sorted(raw["screens"]):
        snapshots.append(write_icon(f"{stem}_screen_{number}.png", raw["screens"][number]))
    record["snapshot"] = snapshots

    return record


# ---------------------------------------------------------------------------
# Index assembly
# ---------------------------------------------------------------------------

def safe_stem(filename: str) -> str:
    stem = Path(filename).stem
    stem = re.sub(r"[^A-Za-z0-9._-]+", "_", stem).strip("._") or "package"
    return stem[:80]


def build_index(packages_dir: Path, out_dir: Path, options: dict) -> tuple[dict, dict]:
    """Scan the SPK directory and produce the index.

    options:
      file_mask, pretty, base_url, defaults   as before
      embed_icons (bool)  collect icons in memory instead of writing dist/icons
                          (used for the single-file bundle published with a release)
      write_output (bool) write dist/index.json + report (default True)

    Returns (index, report). In embed_icons mode the collected icons are stored
    in ``options['embedded_icons']`` as {relative key: bytes}.
    """
    embed_icons = bool(options.get("embed_icons"))
    write_output = options.get("write_output", True)
    embedded_icons: dict[str, bytes] = {}
    options["embedded_icons"] = embedded_icons

    icons_dir = out_dir / "icons"
    if not embed_icons and write_output:
        if icons_dir.exists():
            shutil.rmtree(icons_dir)
        icons_dir.mkdir(parents=True, exist_ok=True)

    for name, key in (("default_icon_72.png", DEFAULT_ICON_72), ("default_icon_120.png", DEFAULT_ICON_120)):
        source = ASSETS / name
        if not source.is_file():
            raise SystemExit(f"missing bundled asset: {source}")
        data = source.read_bytes()
        if embed_icons:
            embedded_icons[key] = data
        elif write_output:
            shutil.copyfile(source, icons_dir / key.split("/", 1)[1])

    used_stems: set[str] = set()

    def write_icon(name: str, data: bytes) -> str:
        key = f"icons/{name}"
        if embed_icons:
            embedded_icons[key] = data
        elif write_output:
            (icons_dir / name).write_bytes(data)
        return key

    files = sorted(packages_dir.glob(options["file_mask"]), key=lambda item: item.name.lower())
    records: list[dict] = []
    report: dict = {
        "generated_at": None,
        "packages_dir": str(packages_dir),
        "ok": 0,
        "skipped": 0,
        "entries": [],
    }

    for path in files:
        entry: dict = {"file": path.name}
        try:
            stem = safe_stem(path.name)
            if stem in used_stems:
                stem = f"{stem}-{hashlib.sha1(path.name.encode('utf-8')).hexdigest()[:8]}"
            used_stems.add(stem)
            record = build_record(path, stem, write_icon)
            records.append(record)
            entry.update({"status": "ok", "package": record["package"], "version": record["version"]})
            report["ok"] += 1
        except (SpkError, OSError) as error:
            entry.update({"status": "skipped", "error": str(error)})
            report["skipped"] += 1
            print(f"  ! {path.name}: {error}", file=sys.stderr)
        report["entries"].append(entry)

    generated_at = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    index = {
        "schema": SCHEMA_VERSION,
        "generated_at": generated_at,
        "base_url": options.get("base_url") or None,
        "defaults": options["defaults"],
        "packages": records,
    }
    report["generated_at"] = generated_at

    if write_output:
        out_dir.mkdir(parents=True, exist_ok=True)
        (out_dir / "index.json").write_text(
            json.dumps(index, ensure_ascii=False, indent=2 if options.get("pretty") else None) + "\n",
            encoding="utf-8",
            newline="\n",
        )
        (out_dir / "index.report.json").write_text(
            json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8", newline="\n"
        )
    return index, report


def build_bundle(index: dict, options: dict) -> dict:
    """Single-file bundle published as a GitHub Release asset.

    The Worker's cron sync reads it to refresh R2: package metadata (including
    the exact md5 computed here) plus every icon as base64, so the Worker never
    has to parse a 171 MB tar or hash it with Web Crypto (which has no MD5).
    """
    icons = options.get("embedded_icons") or {}
    bundle = {
        "schema": BUNDLE_SCHEMA_VERSION,
        "generated_at": index["generated_at"],
        "repo": options.get("repo") or None,
        "tag": options.get("tag") or None,
        "defaults": index["defaults"],
        "packages": index["packages"],
        "icons": {key: base64.b64encode(data).decode("ascii") for key, data in sorted(icons.items())},
    }
    return bundle


# ---------------------------------------------------------------------------
# Upload
# ---------------------------------------------------------------------------

def build_upload_plan(index: dict, out_dir: Path, packages_dir: Path, prefix: str) -> list[tuple[str, Path, str]]:
    """Return (object key, local file, content type) tuples to upload."""
    plan: list[tuple[str, Path, str]] = [
        (f"{prefix}/index.json", out_dir / "index.json", "application/json"),
    ]
    icons_dir = out_dir / "icons"
    if icons_dir.is_dir():
        for icon in sorted(icons_dir.glob("*")):
            if icon.is_file():
                plan.append((f"{prefix}/icons/{icon.name}", icon, "image/png"))
    for record in index["packages"]:
        source = packages_dir / record["file"]
        if source.is_file():
            plan.append((f"{prefix}/packages/{record['file']}", source, "application/octet-stream"))
    return plan


def split_command(command: str) -> list[str]:
    """Split a command string; cmd.exe style quoting is accepted on Windows."""
    parts = shlex.split(command, posix=(os.name != "nt"))
    if os.name == "nt":
        parts = [part[1:-1] if len(part) >= 2 and part[0] == '"' and part[-1] == '"' else part
                 for part in parts]
    return [part for part in parts if part]


def resolve_executable(command: list[str]) -> list[str]:
    """Resolve the executable against PATH/PATHEXT (needed for `npx` on Windows)."""
    if not command or os.name != "nt":
        return command
    found = shutil.which(command[0])
    return [found, *command[1:]] if found else command


def wrangler_put_command(wrangler_cmd: list[str], bucket: str, key: str, path: Path,
                         content_type: str) -> list[str]:
    """Build `wrangler r2 object put` for one object (uses the OAuth login, no S3 keys)."""
    return [
        *wrangler_cmd,
        "r2", "object", "put", f"{bucket}/{key}",
        "--file", str(path),
        "--remote",
        "--content-type", content_type,
    ]


def upload(index: dict, out_dir: Path, packages_dir: Path, prefix: str, bucket: str | None,
           skip_existing: bool, dry_run: bool, backend: str = "s3",
           wrangler_cmd: str = "npx wrangler") -> int:
    plan = build_upload_plan(index, out_dir, packages_dir, prefix)
    total_bytes = sum(path.stat().st_size for _, path, _ in plan)
    print(f"upload plan: {len(plan)} objects, {total_bytes / 1024 / 1024:.1f} MiB -> bucket '{bucket or '$R2_BUCKET'}'")

    if backend == "wrangler":
        return upload_with_wrangler(plan, bucket, dry_run, wrangler_cmd, skip_existing)

    from r2 import R2Error, client_from_env  # local module

    if dry_run:
        for key, path, _ in plan:
            print(f"  PUT {key}  ({path.stat().st_size} bytes)")
        return 0

    client = client_from_env(bucket)
    uploaded = skipped = 0
    for key, path, content_type in plan:
        try:
            if skip_existing and not key.endswith("/index.json"):
                existing = client.head(key)
                if existing is not None and existing == path.stat().st_size:
                    skipped += 1
                    continue
            client.put_file(key, path, content_type)
            uploaded += 1
            print(f"  PUT {key} ({path.stat().st_size} bytes)")
        except R2Error as error:
            print(f"  ! {key}: {error}", file=sys.stderr)
            return 1
    print(f"uploaded {uploaded} objects, skipped {skipped} unchanged")
    return 0


def upload_with_wrangler(plan: list[tuple[str, Path, str]], bucket: str | None, dry_run: bool,
                         wrangler_cmd: str, skip_existing: bool) -> int:
    """Upload through the Wrangler CLI, reusing `wrangler login` instead of R2 S3 keys."""
    if not bucket:
        print("wrangler backend needs a bucket: pass --bucket or set R2_BUCKET", file=sys.stderr)
        return 2
    if skip_existing:
        print("note: --skip-existing is not supported by the wrangler backend; uploading everything")
    command = resolve_executable(split_command(wrangler_cmd))
    if not command:
        print("--wrangler-cmd is empty", file=sys.stderr)
        return 2

    uploaded = 0
    for key, path, content_type in plan:
        argv = wrangler_put_command(command, bucket, key, path, content_type)
        if dry_run:
            print("  " + " ".join(argv))
            continue
        result = subprocess.run(argv, capture_output=True, text=True)
        if result.returncode != 0:
            detail = (result.stderr or result.stdout or "").strip().splitlines()
            print(f"  ! {key}: {detail[-1] if detail else 'wrangler failed'}", file=sys.stderr)
            return 1
        uploaded += 1
        print(f"  PUT {key} ({path.stat().st_size} bytes)")
    if dry_run:
        return 0
    print(f"uploaded {uploaded} objects via wrangler")
    return 0


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------

def load_config(path: Path | None) -> dict:
    """Load the optional sspks.config.json (CLI flags win over config values)."""
    config: dict = {
        "defaults": {
            "download_count": 0,
            "maintainer": "Community Repository",
            "maintainer_url": "",
            "distributor": "Community Repository",
            "distributor_url": "",
            "support_url": "",
            "changelog_ad_enabled": False,
            "changelog_ad": "",
        },
    }
    if path is None:
        return config
    data = json.loads(path.read_text(encoding="utf-8"))
    unknown = set(data) - {"defaults", "packages_dir", "out_dir", "file_mask", "base_url", "prefix", "bucket"}
    if unknown:
        raise SystemExit(f"unknown config keys: {', '.join(sorted(unknown))}")
    config.update({key: value for key, value in data.items() if key != "defaults"})
    config["defaults"].update(data.get("defaults", {}))
    return config


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Build the static SPK index for the R2/Workers package source.")
    parser.add_argument("--packages", type=Path, help="directory containing .spk files (default: ./packages)")
    parser.add_argument("--out", type=Path, help="output directory (default: ./dist)")
    parser.add_argument("--config", type=Path, help="optional JSON config (default: ./sspks.config.json if present)")
    parser.add_argument("--base-url", default=None, help="public URL, stored in index.json for reference")
    parser.add_argument("--file-mask", default=None, help="glob used to find packages (default: *.spk)")
    parser.add_argument("--pretty", action="store_true", help="pretty-print index.json")
    parser.add_argument("--upload", action="store_true", help="upload dist/ and packages/ to R2")
    parser.add_argument("--dry-run", action="store_true", help="print the upload plan without uploading")
    parser.add_argument("--bundle", type=Path, default=None,
                        help="写出单文件索引包（发布为 Release 资产，供 Worker 定时同步；含内嵌图标，不写 dist）")
    parser.add_argument("--repo", default="", help="写进 bundle 的仓库名，例如 NeeYoonc/dsh-nas")
    parser.add_argument("--tag", default="", help="写进 bundle 的 Release tag")
    parser.add_argument("--skip-existing", action="store_true", help="skip objects R2 already has (same size)")
    parser.add_argument("--upload-backend", choices=["s3", "wrangler"], default="s3",
                        help="s3: R2 S3 API with R2_* keys (default); wrangler: reuse `wrangler login`")
    parser.add_argument("--wrangler-cmd", default="npx wrangler",
                        help="wrangler executable for the wrangler backend (default: npx wrangler)")
    parser.add_argument("--bucket", default=None, help="R2 bucket (default: $R2_BUCKET)")
    parser.add_argument("--prefix", default=None, help="object key prefix (default: spk)")
    parser.add_argument("--clean", action="store_true", help="remove the output directory first")
    args = parser.parse_args(argv)

    config_path = args.config
    if config_path is None:
        candidate = ROOT / "sspks.config.json"
        config_path = candidate if candidate.is_file() else None
    config = load_config(config_path)

    packages_dir = (args.packages or Path(config.get("packages_dir") or ROOT / "packages")).resolve()
    out_dir = (args.out or Path(config.get("out_dir") or ROOT / "dist")).resolve()
    file_mask = args.file_mask or str(config.get("file_mask") or "*.spk")
    base_url = (args.base_url if args.base_url is not None else str(config.get("base_url") or "")).strip()
    prefix = (args.prefix if args.prefix is not None else str(config.get("prefix") or "spk")).strip("/")
    bucket = args.bucket or config.get("bucket")

    if args.bundle is not None and (args.upload or args.dry_run):
        print("--bundle 与 --upload 不能同时使用（bundle 由 Worker 定时同步拉取）", file=sys.stderr)
        return 2
    if not packages_dir.is_dir():
        print(f"packages directory not found: {packages_dir}", file=sys.stderr)
        return 2
    if args.clean and out_dir.exists():
        shutil.rmtree(out_dir)

    print(f"scanning {packages_dir}")
    bundle_mode = args.bundle is not None
    options = {
        "file_mask": file_mask,
        "pretty": args.pretty,
        "base_url": base_url,
        "defaults": config["defaults"],
        "embed_icons": bundle_mode,
        "write_output": not bundle_mode,
    }
    index, report = build_index(packages_dir, out_dir, options)
    print(f"indexed {report['ok']} packages, skipped {report['skipped']}")

    if bundle_mode:
        bundle = build_bundle(index, {
            "embedded_icons": options["embedded_icons"],
            "repo": args.repo.strip(),
            "tag": args.tag.strip(),
        })
        args.bundle.parent.mkdir(parents=True, exist_ok=True)
        args.bundle.write_text(
            json.dumps(bundle, ensure_ascii=False, indent=2 if args.pretty else None) + "\n",
            encoding="utf-8",
            newline="\n",
        )
        print(f"wrote bundle {args.bundle} ({len(bundle['icons'])} icons, {len(bundle['packages'])} packages)")
        return 0

    if args.upload or args.dry_run:
        result = upload(index, out_dir, packages_dir, prefix, bucket,
                        args.skip_existing, args.dry_run,
                        backend=args.upload_backend, wrangler_cmd=args.wrangler_cmd)
        if result != 0:
            return result
    else:
        print(f"wrote {out_dir / 'index.json'}")
        print("next: upload with --upload, or use rclone/wrangler (see README)")
    return 0


if __name__ == "__main__":
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    raise SystemExit(main())
