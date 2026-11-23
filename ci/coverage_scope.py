"""Shared coverage-scope parsing: the BW-xx ticket scopes from todo.md.

The coverage mappers (map-coverage, map-entry) both index a reference-side
item by the ticket whose `源码` line names it. The scopes are parsed from
todo.md itself, so a derived index always points at a heading that exists.
"""

import os
import re

ROOTS = ("lib", "server", "client", "tools", "common", "examples")

HEADING_RE = re.compile(r"^#{3,4}\s+(BW-[0-9]+(?:-[0-9]+)*)")
SCOPE_RE = re.compile(r"^源码")
TOKEN_RE = re.compile(r"`([^`]+)`")
BRACE_RE = re.compile(r"\{([^{}]*)\}")


def expand_braces(text):
    """`lib/{a,b}/` -> [lib/a, lib/b] (one level; the scopes never nest)."""
    m = BRACE_RE.search(text)
    if not m:
        return [text]
    out = []
    for alt in m.group(1).split(","):
        out.extend(expand_braces(text[:m.start()] + alt + text[m.end():]))
    return out


def scope_dir(token):
    """A scope token normalized to a static source directory, or None.

    A bare root is a scope (`tools/` -> tools, `examples/` -> examples); file
    names and glob tails drop to their directory (`lib/server/bwservice.hpp`
    -> lib/server, `server/dbapp/*login*` -> server/dbapp); tokens outside the
    six source roots (links, bare file names, ../ paths) are not scopes.
    """
    t = token.strip().rstrip("/")
    if t.split("/", 1)[0] not in ROOTS:
        return None
    last = t.rsplit("/", 1)[1] if "/" in t else ""
    if last and ("." in last or "*" in last):
        t = t.rsplit("/", 1)[0]
    return t or None


def parse_scopes(todo_path):
    """{ticket: [source dir, ...]} from each heading's 源码 line."""
    scopes = {}
    if not os.path.isfile(todo_path):
        return None
    ticket = None
    with open(todo_path, encoding="utf-8") as f:
        for raw in f:
            m = HEADING_RE.match(raw)
            if m:
                ticket = m.group(1)
                continue
            if ticket and SCOPE_RE.match(raw):
                for tok in TOKEN_RE.findall(raw):
                    for one in expand_braces(tok):
                        d = scope_dir(one)
                        if d and d not in scopes.setdefault(ticket, []):
                            scopes[ticket].append(d)
                ticket = None  # one scope line per heading
    return scopes


def scope_tokens(todo_path):
    """{ticket: [raw backticked tokens, ...]} for tokens outside the roots.

    The root-relative scopes are directories; the rest (../fantasydemo/, a
    scripts path) are still a ticket's named source, so the entry mapper can
    index a file by the raw token that mentions it.
    """
    tokens = {}
    if not os.path.isfile(todo_path):
        return None
    ticket = None
    with open(todo_path, encoding="utf-8") as f:
        for raw in f:
            m = HEADING_RE.match(raw)
            if m:
                ticket = m.group(1)
                continue
            if ticket and SCOPE_RE.match(raw):
                for tok in TOKEN_RE.findall(raw):
                    if tok not in tokens.setdefault(ticket, []):
                        tokens[ticket].append(tok)
                ticket = None
    return tokens


def tickets_for_dir(directory, scopes):
    """The tickets whose scope dir covers a source directory."""
    return sorted(t for t, dirs in scopes.items()
                  if any(d == directory or directory.startswith(d + "/")
                         for d in dirs))


def tickets_for_token(entry, tokens):
    """The tickets whose raw scope token mentions an entry path.

    The token is matched brace-expanded (`common/{a.cpp,b.cpp}` -> both files)
    and by its directory or basename, so a file entry resolves to every scope
    that names its directory (`common/` -> BW-03) or the file itself
    (`common/py_network.cpp` -> BW-02).
    """
    hits = []
    for t, toks in tokens.items():
        for tok in toks:
            for one in expand_braces(tok.strip("`")):
                base = one.rstrip("/")
                name = os.path.basename(base) if "/" in base else ""
                if entry == base or entry.startswith(base + "/") \
                        or (name and (entry == name or entry.endswith("/" + name))):
                    hits.append(t)
                    break
    return sorted(set(hits))
