#!/usr/bin/env python3
"""Verify that every internal link in the Boundless docs site resolves.

VitePress serves `docs/` as the site root, so an absolute link "/guide/x"
resolves to docs/guide/x.md (or .html / index.md) and a bare relative link
resolves against the directory of the file that contains it. An asset link
must exist under docs/public/.

VitePress's own dead-link check only sees what is rendered, so this script
also walks `.vitepress/config.mjs` and validates every `link:` value there:
a nav or sidebar entry pointing at a missing page must fail the gate too.
Comments in that file are stripped first, because the docstring documents a
locale switch that is not enabled and would otherwise look like a link.

`.vitepress/`, `node_modules/`, `dist/` and `.git/` are never scanned.

Usage:  python3 scripts/check-docs-links.py [docs]
Exit:   0 when no dead links, 1 otherwise.
"""

import re
import sys
from pathlib import Path
from urllib.parse import unquote

# Directories that are never part of the site.
SKIP_DIRS = frozenset({".vitepress", "node_modules", "dist", ".git"})

PAGES = (".html", ".md", ".mdx")
INLINE = re.compile(r"(?<!!)\[[^\]]*\]\(([^)\s]+)(?:\s+\"[^\"]*\")?\)")
IMAGE = re.compile(r"!\[[^\]]*\]\(([^)\s]+)(?:\s+\"[^\"]*\")?\)")
REF = re.compile(r"^\s*\[[^\]]+\]:\s*<?([^>\s]+)>?(?:\s+[^\n]*)?$")
EXTERNAL = re.compile(r"^[a-zA-Z][a-zA-Z0-9+.-]*:")
# `link: "/guide/install"` and `link: "/"` in .vitepress/config.mjs
CONFIG_LINK = re.compile(r"""link\s*:\s*["'](/[^"']*)["']""")


def targets(markdown: str):
    """Every link destination in the document, reference definitions included."""
    for rx in (INLINE, IMAGE):
        for m in rx.finditer(markdown):
            yield m.group(1)
    for line in markdown.splitlines():
        m = REF.match(line)
        if m:
            yield m.group(1)


def candidate_paths(base: Path, doc: Path, raw: str):
    """Files VitePress would try for this destination, in order."""
    dest = unquote(raw.split("?", 1)[0].split("#", 1)[0].strip())
    if not dest or dest.startswith("#") or dest.startswith("//") \
            or EXTERNAL.match(dest):
        return []

    # Absolute links are rooted at the docs site root, not at the file.
    root = base if dest.startswith("/") else doc.parent
    rel = dest.lstrip("/")

    # The site root itself is the landing page.
    if rel == "":
        return [base / "index.md", base / "index.html"]

    full = root / rel
    suffix = full.suffix.lower()

    # Anything with a suffix that is not a page suffix is a static resource,
    # served from public/. Example configs and scripts land here.
    if suffix and suffix not in PAGES:
        return [full, base / "public" / rel]

    out = [full] if suffix in PAGES else []
    out += [full.with_suffix(".md"), full / "index.md", full.with_suffix(".html")]
    return out


def is_dead(cands):
    return cands and not any(c.is_file() for c in cands)


def strip_js_comments(text: str) -> str:
    """Drop `//` and `/* */` comments, leaving string literals untouched.

    Config comments legitimately mention `link: "/zh/"` for the locale
    switch that is not enabled, so a comment-aware scan is required or the
    checker reports pages that do not exist.
    """
    out = []
    i = 0
    n = len(text)
    while i < n:
        c = text[i]
        if c in "\"'`":
            quote = c
            out.append(c)
            i += 1
            while i < n:
                out.append(text[i])
                if text[i] == "\\":
                    if i + 1 < n:
                        out.append(text[i + 1])
                        i += 1
                elif text[i] == quote:
                    i += 1
                    break
                elif text[i] == "\n":
                    i += 1
                    break
                i += 1
            continue
        if c == "/" and i + 1 < n and text[i + 1] == "/":
            while i < n and text[i] != "\n":
                i += 1
            continue
        if c == "/" and i + 1 < n and text[i + 1] == "*":
            i += 2
            while i + 1 < n and not (text[i] == "*" and text[i + 1] == "/"):
                i += 1
            i += 2
            continue
        out.append(c)
        i += 1
    return "".join(out)


def markdown_files(base: Path):
    """Site markdown, skipping build output and dependency trees."""
    for doc in sorted(base.rglob("*.md")):
        if any(part in SKIP_DIRS for part in doc.parts):
            continue
        yield doc


def main():
    base = Path(sys.argv[1] if len(sys.argv) > 1 else "docs").resolve()
    if not base.is_dir():
        print(f"not a docs directory: {base}")
        return 1

    dead = []
    checked = 0

    for doc in markdown_files(base):
        text = doc.read_text(encoding="utf-8", errors="replace")
        where = doc.relative_to(base)
        for raw in targets(text):
            cands = candidate_paths(base, doc, raw)
            if not cands:
                continue
            checked += 1
            if is_dead(cands):
                dead.append((str(where), raw,
                             ", ".join(str(c.relative_to(base)) for c in cands)))

    # Nav and sidebar entries live in config, not in markdown.
    config = base / ".vitepress" / "config.mjs"
    if config.is_file():
        source = strip_js_comments(
            config.read_text(encoding="utf-8", errors="replace"))
        for raw in CONFIG_LINK.findall(source):
            cands = candidate_paths(base, base, raw)
            if not cands:
                continue
            checked += 1
            if is_dead(cands):
                dead.append((".vitepress/config.mjs", raw,
                             ", ".join(str(c.relative_to(base)) for c in cands)))

    for where, raw, cands in dead:
        print(f"DEAD  {where} -> {raw}  (tried: {cands})")
    print(f"{checked} internal links checked, {len(dead)} dead")
    return 1 if dead else 0


if __name__ == "__main__":
    sys.exit(main())