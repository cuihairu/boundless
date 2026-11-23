#!/usr/bin/env python3
"""Scan the reference BigWorld runtime install tree for BW-00-02-01.

Captures the runtime environment facts the acceptance asks for: source commit,
binary SHA256 digests, dynamic linkage (which system shared libraries each
binary pulls), the build parameters (platform/config/compiler), and the Python
and database versions (embedded interpreter, vcpkg port set, system libs).

Best-effort: every probe has a fallback to None rather than aborting, so a
partial tree still yields a usable inventory.

Usage: runtime_env_scan.py BIGWORLD_ROOT [OUTPUT_JSON]
"""
import hashlib
import json
import os
import re
import subprocess
import sys

BIN_SUBDIRS = ("server", "tools", "examples", "unit_tests")


def run(cmd):
    try:
        return subprocess.run(
            cmd, capture_output=True, text=True, timeout=30
        ).stdout.strip()
    except Exception:
        return None


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def probe_version_header(vcpkg_root, relpath, regex):
    path = os.path.join(vcpkg_root, relpath)
    if not os.path.isfile(path):
        return None
    try:
        with open(path, encoding="utf-8", errors="replace") as fh:
            for line in fh:
                m = regex.search(line)
                if m:
                    return m.group(1)
    except Exception:
        return None
    return None


def collect_binaries(bin_root):
    """Yield (relpath, abspath) for every regular file in the install tree."""
    for dirname in BIN_SUBDIRS:
        base = os.path.join(bin_root, dirname)
        if not os.path.isdir(base):
            continue
        for root, _dirs, files in os.walk(base):
            for fn in sorted(files):
                path = os.path.join(root, fn)
                if not os.path.isfile(path) or os.path.islink(path):
                    continue
                yield os.path.relpath(path, bin_root), path


def ldd_shared(path):
    """List system shared libraries a binary pulls, or [] when fully static."""
    out = run(["ldd", path])
    libs = []
    for line in (out or "").splitlines():
        m = re.search(r"^\s*(\S+\.so\.?\d*)\s*=>\s*(/\S+)", line)
        if m and not m.group(2).startswith("/lib64/ld-"):
            libs.append(m.group(1))
    return libs


def main():
    if len(sys.argv) < 2:
        print(__doc__)
        return 1
    root = sys.argv[1]
    programming = os.path.join(root, "programming", "bigworld")
    bin_root = os.path.join(root, "game", "bin", "server", "el7")

    # Source commit.
    head = run(["git", "-C", root, "rev-parse", "HEAD"])
    head_short = run(["git", "-C", root, "rev-parse", "--short", "HEAD"])
    head_subj = run(["git", "-C", root, "log", "-1", "--format=%s"])
    head_date = run(["git", "-C", root, "log", "-1", "--format=%ci"])

    # Revision stamp.
    rev = None
    rev_file = os.path.join(programming, "build", "el7", "build",
                            "build_revision.generated.hpp")
    if os.path.isfile(rev_file):
        m = re.search(r"REVISION_NUMBER\s+(-?\d+)", open(rev_file).read())
        rev = m.group(1) if m else None

    # Build parameters from the Makefile (BW_CONFIG default hybrid, no suffix).
    makefile = os.path.join(programming, "build", "make", "Makefile")
    bw_config = None
    platform_config = None
    if os.path.isfile(makefile):
        text = open(makefile).read()
        m = re.search(r"BW_CONFIG_HYBRID\s*:?=\s*(\w+)", text)
        hybrid = m.group(1) if m else "hybrid"
        m = re.search(r"BW_CONFIG_DEFAULT\s*:?=\s*\$\(BW_CONFIG_HYBRID\)", text)
        bw_config = hybrid if m else None
        platform_config = "el7"  # hybrid config folds the suffix away (Makefile:86)

    # Compiler stamp and embedded Python version from a representative binary.
    cellapp = os.path.join(bin_root, "server", "cellapp")
    gcc = None
    py_embedded = None
    if os.path.isfile(cellapp):
        out = run(["strings", "-a", cellapp])
        m = re.search(r"GCC:\s*\(([^)]*)\)\s*([0-9.]+)", out or "")
        gcc = f"{m.group(1)} {m.group(2)}" if m else None
        # Standalone version, not a fragment of a dotted OID like the OpenSSL
        # OBJ identifiers ("1.3.14.3.2.17" - OpenSSL 3.x strings carry them).
        m = re.search(r"(?<![0-9.])3\.1[0-9]\.[0-9]+(?![0-9.])", out or "")
        py_embedded = m.group(0) if m else None

    # vcpkg port versions from the build-time install tree headers.
    vcpkg_root = os.path.join(
        programming, "build", "el7", "third_party", "vcpkg_installed", "x64-linux-bw")
    ports = {
        "openssl": probe_version_header(vcpkg_root, "include/openssl/opensslv.h",
                                        re.compile(r'OPENSSL_VERSION_TEXT\s+"([^"]+)"')),
        "curl": probe_version_header(vcpkg_root, "include/curl/curlver.h",
                                     re.compile(r'LIBCURL_VERSION\s+"([^"]+)"')),
        "jsoncpp": probe_version_header(vcpkg_root, "include/json/version.h",
                                        re.compile(r'JSONCPP_VERSION_STRING\s+"([^"]+)"')),
        "zlib": probe_version_header(vcpkg_root, "include/zlib.h",
                                     re.compile(r'ZLIB_VERSION\s+"([^"]+)"')),
        "libpng": probe_version_header(vcpkg_root, "include/png.h",
                                       re.compile(r'PNG_LIBPNG_VER_STRING\s+"([^"]+)"')),
        "sqlite3": probe_version_header(vcpkg_root, "include/sqlite3.h",
                                        re.compile(r'SQLITE_VERSION\s+"([^"]+)"')),
        "bzip2": probe_version_header(vcpkg_root, "include/bzlib.h",
                                      re.compile(r'bzip2/libbzip2\s+version\s+([0-9.]+)')),
    }

    # System versions.
    system = {
        "python3": run(["python3", "--version"]) or None,
        "openssl": run(["openssl", "version"]) or None,
        "mysql_config": run(["mysql_config", "--version"]) or None,
        "sqlite3": (run(["sqlite3", "--version"]) or "").split()[0:1] or None,
    }

    # MongoDB legacy driver version from the vendored tarball name.
    mongo_dir = os.path.join(programming, "third_party", "mongodb", "mongo_cxx_driver")
    mongo_driver = None
    if os.path.isdir(mongo_dir):
        for fn in os.listdir(mongo_dir):
            m = re.search(r"legacy.*?([0-9]+\.[0-9]+\.[0-9]+)", fn)
            if m:
                mongo_driver = m.group(1)
                break

    binaries = []
    for rel, path in sorted(collect_binaries(bin_root)):
        with open(path, "rb") as fh:
            is_elf = fh.read(4) == b"\x7fELF"
        binaries.append({
            "path": rel,
            "kind": "elf" if is_elf else "data",
            "sha256": sha256(path),
            "size": os.path.getsize(path),
            "shared_libs": ldd_shared(path) if is_elf else [],
        })

    data = {
        "source_commit": head,
        "source_commit_short": head_short,
        "source_commit_subject": head_subj,
        "source_commit_date": head_date,
        "revision_number": rev,
        "build": {
            "platform": "el7",
            "config": bw_config,
            "platform_config": platform_config,
            "compiler_stamp": gcc,
            "cpp_standard": "c++23",
        },
        "python_embedded": py_embedded,
        "vcpkg_ports": ports,
        "system": system,
        "mongodb_driver": mongo_driver,
        "binaries": binaries,
    }

    if len(sys.argv) >= 3:
        with open(sys.argv[2], "w", encoding="utf-8") as fh:
            json.dump(data, fh, indent=1, sort_keys=True)

    print(f"commit: {head_short} {head_subj}")
    print(f"binaries: {len(binaries)}  (sha256 computed)")
    print(f"build: platform={data['build']['platform']} config={bw_config} "
          f"gcc={gcc} python={py_embedded}")
    print(f"vcpkg: " + ", ".join(f"{k}={v}" for k, v in ports.items() if v))
    return 0


if __name__ == "__main__":
    sys.exit(main())
