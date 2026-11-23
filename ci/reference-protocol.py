#!/usr/bin/env python3
"""Enumerate the messages every reference Mercury interface declares.

Every message-declaring macro, however wrapped, reduces in the reference to one
canonical registration:

    MERCURY_MESSAGE( NAME, STYLE, PARAM, HANDLER )
      -> const Mercury::InterfaceElement & NAME =
               gMinder.add( #NAME, Mercury::STYLE, PARAM, ... );

`InterfaceMinder::add` sets the id to `elements_.size()`, so a message's id is
exactly its position in declaration order of the compiled translation unit
(lib/network/interface_minder.cpp). That is the whole point of this script: the
ids are read off what `gMinder.add` actually receives, not off the macro that
was typed.

There is no C preprocessor available, so this script resolves the `#if` /
`#ifdef` / `#include` / `#define` / `#undef` constructs that actually occur in
the interface files against the macro set the reference itself establishes for
them:

  DEFINE_INTERFACE_HERE set        the TU that registers the messages; it
                                   selects the gMinder.add branch
  DEFINE_SERVER_HERE unset         changes only the handler argument, never the
                                   declaration order
  VOLATILE_POSITIONS_ARE_ABSOLUTE 0 from lib/network/msgtypes.hpp; it decides
                                   which avatarUpdate variants exist

Only the handful of headers that define message macros are pulled in; the rest
declare types those macros reference but never a message. Expansion happens at
the point each statement is emitted, because `common_client_interface.hpp`
`#undef`s its aliases at its own end — a batch expansion after preprocessing
would lose every message that fragment contributes.

Usage:
  ci/reference-protocol.py REFERENCE_ROOT [OUT_TSV]

Emits one tab-separated row per message:
  interface  message  id  length_style  length_param  direction  source_file  line
`line` is the declaration site in the source file, 0 when the name is built by
`##` pasting (macro-generated) and so has no literal line to point at.
"""

import os
import re
import sys

# Headers that define message macros. Anything else is opaque.
INCLUDE_WHITELIST = {
    "interface_macros.hpp",
    "common_interface_macros.hpp",
    "common_baseapp_interface.hpp",
    "common_client_interface.hpp",
    "reviver_subject.hpp",
    "common_message_handlers.hpp",
}

# The 11 production interfaces and the direction each speaks.
#
# Direction is the flow the interface file is written for, read off its own
# handler target:
#   client_to_server   the client sends it   (lib/connection/login_interface,
#                                            baseapp_ext_interface)
#   server_to_client   the server sends it   (lib/connection/client_interface:
#                                            every message is routed to
#                                            ServerConnection::NAME)
#   server_internal    exchanged between server components (lib/db/*, server/*)
FILES = [
    ("lib/connection/login_interface.hpp", "client_to_server"),
    ("lib/connection/baseapp_ext_interface.hpp", "client_to_server"),
    ("lib/connection/client_interface.hpp", "server_to_client"),
    ("lib/db/dbapp_interface.hpp", "server_internal"),
    ("lib/db/dbappmgr_interface.hpp", "server_internal"),
    ("server/baseapp/baseapp_int_interface.hpp", "server_internal"),
    ("server/baseappmgr/baseappmgr_interface.hpp", "server_internal"),
    ("server/cellapp/cellapp_interface.hpp", "server_internal"),
    ("server/cellappmgr/cellappmgr_interface.hpp", "server_internal"),
    ("server/loginapp/login_int_interface.hpp", "server_internal"),
    ("server/reviver/reviver_interface.hpp", "server_internal"),
]

# Message names are declared through these families; used only to locate the
# declaration site of a non-pasted name.
_MSG_MACRO = (
    r'(?i)\b(?:MERCURY_[A-Z_]*_MESSAGE|BW_(?:BEGIN_STRUCT|STREAM|BIG_STREAM|EMPTY)'
    r'_MSG(?:_EX)?|MF_[A-Z_]*_MSG(?:_EX)?|MF_BEGIN_[A-Z_]*_MSG|BW_REVIVER_MSGS)'
)

PYTHON_KEYWORDS = {"not", "and", "or", "True", "False", "None", "in", "is",
                   "if", "else", "elif", "def", "class", "lambda"}


def strip_comments(src):
    src = re.sub(r"/\*.*?\*/", lambda m: "\n" * m.group(0).count("\n"), src,
                 flags=re.S)
    return re.sub(r"//[^\n]*", "", src)


def join_cont(text):
    """Join backslash line-continuations, as the C preprocessor does."""
    out, cur = [], ""
    for raw in text.split("\n"):
        line = raw.rstrip()
        if line.endswith("\\"):
            cur += line[:-1] + " "
        else:
            out.append(cur + line)
            cur = ""
    if cur:
        out.append(cur)
    return "\n".join(out)


def split_args(s):
    """Top-level argument split, honoring (), [] and <> nesting."""
    out, cur, depth = [], "", 0
    for ch in s:
        if ch in "([{<":
            depth += 1
        elif ch in ")]}>":
            depth -= 1
        if ch == "," and depth == 0:
            out.append(cur.strip())
            cur = ""
        else:
            cur += ch
    if cur.strip():
        out.append(cur.strip())
    return out


def subst(body, params, args, obj):
    """Substitute a macro body: stringize (#X), paste (A##B), then textually."""
    amap = dict(zip(params, args))
    for p in sorted(params, key=len, reverse=True):
        if not p:
            continue
        # #X stringize. Lookbehind keeps this away from the second '#' of '##',
        # which is pasting, not stringize.
        body = re.sub(r"(?<!#)#\s*\b" + re.escape(p) + r"\b",
                      lambda m: '"%s"' % amap.get(p, p), body)
    for p in sorted(params, key=len, reverse=True):
        if p:
            body = re.sub(r"(?<![A-Za-z0-9_])" + re.escape(p) + r"(?![A-Za-z0-9_])",
                           lambda m: amap.get(p, p), body)
    out = []
    for tok in body.split():
        if "##" in tok:
            joined = "".join(tok.split("##"))
            tok = obj.get(joined, joined)
        out.append(tok)
    return " ".join(out)


class PP:
    """A just-enough C preprocessor for the interface files."""

    def __init__(self, roots):
        self.roots = roots
        self.obj = {}    # name -> replacement text ("")
        self.func = {}   # name -> (params, body)
        self.seen = set()

    # -- macro state --------------------------------------------------------
    def isdef(self, name):
        return name in self.obj or name in self.func

    def value(self, name):
        """#if value: numeric, otherwise truthiness (defined-but-empty is true)."""
        if name in self.obj:
            v = self.obj[name].strip()
            if v == "":
                return "1"
            if re.match(r"^-?\d+$", v):
                return v
            return "1"
        if name in self.func:
            return "1"
        return "0"

    def eval_expr(self, e):
        e = re.sub(r"defined\s*\(\s*(\w+)\s*\)",
                   lambda m: str(int(self.isdef(m.group(1)))), e)
        e = re.sub(r"defined\s+(\w+)",
                   lambda m: str(int(self.isdef(m.group(1)))), e)
        e = re.sub(r"!\s*", " not ", e)
        e = re.sub(r"&&", " and ", e)
        e = re.sub(r"\|\|", " or ", e)
        e = re.sub(r"\b[A-Za-z_]\w*\b",
                   lambda m: (m.group(0) if m.group(0) in PYTHON_KEYWORDS
                              else self.value(m.group(0))), e)
        try:
            return bool(eval(e, {"__builtins__": {}}, {}))
        except Exception:
            return False

    # -- includes ------------------------------------------------------------
    def resolve(self, path, inc_dir):
        for c in ([os.path.join(inc_dir, path)]
                  + [os.path.join(r, path) for r in self.roots]):
            c = os.path.normpath(c)
            if os.path.isfile(c):
                return c
        return None

    def process_file(self, path):
        path = os.path.abspath(path)
        if path in self.seen:
            return ""
        self.seen.add(path)
        return self.process(strip_comments(open(path).read()),
                            os.path.dirname(path))

    # -- expansion -----------------------------------------------------------
    def expand(self, text, maxiter=800):
        for _ in range(maxiter):
            changed = False
            for name in list(self.func):
                m = re.search(r"\b" + re.escape(name) + r"\s*\(", text)
                if not m:
                    continue
                depth, i = 0, m.end() - 1
                while i < len(text):
                    if text[i] == "(":
                        depth += 1
                    elif text[i] == ")":
                        depth -= 1
                        if depth == 0:
                            break
                    i += 1
                if depth != 0:
                    return text
                params, body = self.func[name]
                text = (text[:m.start()]
                        + subst(body, params, split_args(text[m.end():i]),
                                self.obj)
                        + text[i + 1:])
                changed = True
                break
            if changed:
                continue
            for name in list(self.obj):
                t = self.obj[name]
                m = re.search(r"(?<![A-Za-z0-9_])" + re.escape(name)
                              + r"(?![A-Za-z0-9_])", text)
                if not m:
                    continue
                text = text[:m.start()] + t + text[m.end():]
                changed = True
                break
            if not changed:
                break
        return text

    # -- the walk ------------------------------------------------------------
    def process(self, text, inc_dir):
        text = join_cont(text)
        out, stack, carry = [], [], ""
        active = lambda: all(s["active"] for s in stack)

        def flush(buf):
            s = buf.strip()
            if s:
                out.append(self.expand(s) if "(" in s else s)

        for raw in text.split("\n"):
            line = raw.strip()
            if line.startswith("#"):
                if carry:
                    flush(carry)
                    carry = ""
                m = re.match(r"(\w+)(.*)", line[1:].lstrip(), re.S)
                if not m:
                    continue
                dname, darg = m.group(1), m.group(2).strip()
                if dname == "if":
                    v = self.eval_expr(darg)
                    stack.append({"taken": v, "active": v})
                elif dname == "ifdef":
                    v = self.isdef(darg)
                    stack.append({"taken": v, "active": v})
                elif dname == "ifndef":
                    v = not self.isdef(darg)
                    stack.append({"taken": v, "active": v})
                elif dname == "elif" and stack:
                    top = stack[-1]
                    v = (not top["taken"]) and self.eval_expr(darg)
                    top["active"] = v
                    top["taken"] = top["taken"] or v
                elif dname == "else" and stack:
                    top = stack[-1]
                    top["active"] = not top["taken"]
                    top["taken"] = True
                elif dname == "endif":
                    if stack:
                        stack.pop()
                elif dname == "include":
                    mi = re.match(r'"([^"]+)"', darg)
                    if mi and os.path.basename(mi.group(1)) in INCLUDE_WHITELIST \
                            and active():
                        p = self.resolve(mi.group(1), inc_dir)
                        if p:
                            out.append(self.process_file(p))
                elif dname == "define" and active():
                    md = re.match(r"(\w+)(\(([^)]*)\))?(.*)", darg, re.S)
                    if md:
                        if md.group(2):
                            self.func[md.group(1)] = (split_args(md.group(3)),
                                                      md.group(4))
                        else:
                            self.obj[md.group(1)] = md.group(4).strip()
                elif dname == "undef" and active():
                    self.obj.pop(darg, None)
                    self.func.pop(darg, None)
                elif active():
                    out.append(raw)
                continue
            if not active():
                continue
            if line and _unbalanced(carry + line):
                carry += line
                continue
            if carry:
                carry += line
                if not _unbalanced(carry):
                    flush(carry)
                    carry = ""
                continue
            flush(line)
        if carry:
            flush(carry)
        return "\n".join(out)


def _unbalanced(s):
    d = 0
    for ch in s:
        if ch == "(":
            d += 1
        elif ch == ")":
            d -= 1
    return d != 0


_ADD = re.compile(r'gMinder\.add\(\s*"(\w+)"\s*,\s*Mercury::(\w+_MESSAGE)'
                  r"\s*,\s*([^,]+),")


def messages_for(text):
    """The (name, length_style, length_param) triples gMinder.add receives."""
    return [(m.group(1), m.group(2), m.group(3).strip())
            for m in _ADD.finditer(text)]


def decl_line(src, name):
    """Line (1-based) where `name` is declared, or 0 if macro-pasted."""
    for i, line in enumerate(src.split("\n"), 1):
        if re.search(r"\b" + re.escape(name) + r"\b", line) \
                and re.search(_MSG_MACRO, line, re.I):
            return i
    return 0


def include_sources(ref, top_rel):
    """(rel_path, src) for an interface file and the whitelisted includes it
    pulls in, in order. A name can be declared in a fragment such as
    common_client_interface.hpp rather than in the interface file itself."""
    out, seen = [], set()

    def walk(rel):
        if rel in seen:
            return
        seen.add(rel)
        path = os.path.join(ref, rel)
        if not os.path.isfile(path):
            return
        src = open(path).read()
        out.append((rel, src))
        for m in re.finditer(r'^\s*#\s*include\s+"([^"]+)"',
                             src, re.M):
            if os.path.basename(m.group(1)) not in INCLUDE_WHITELIST:
                continue
            resolved = None
            for c in ([os.path.join(os.path.dirname(rel), m.group(1))]
                      + [os.path.join(r, m.group(1)) for r in root_dirs(ref)]):
                if os.path.isfile(c):
                    resolved = c
                    break
            if resolved:
                walk(os.path.relpath(resolved, ref))

    walk(top_rel)
    return out


def decl_site(ref, top_rel, name):
    """(file, line) of the first declaration of `name`, or ('', 0)."""
    for rel, src in include_sources(ref, top_rel):
        ln = decl_line(src, name)
        if ln:
            return rel, ln
    return "", 0


def root_dirs(ref):
    dirs = ["", "lib", "lib/connection", "lib/network", "lib/server", "lib/db",
            "lib/math", "lib/cstdmf", "lib/entitydef", "server",
            "server/baseapp", "server/loginapp", "server/cellapp"]
    return [os.path.join(ref, d) for d in dirs
            if os.path.isdir(os.path.join(ref, d))]


def enumerate_interfaces(ref):
    """Return (rows, missing_files). Each row is a message in a production
    interface, with its 0-based id, length style, direction and source.

    `CI_PROTOCOL_FILES` names a manifest of `rel<TAB>direction` lines and is the
    seam the self-test uses to plant a minimal interface; it also keeps the lane
    honest if the interface set in the reference moves."""
    table = FILES
    manifest = os.environ.get("CI_PROTOCOL_FILES")
    if manifest and os.path.isfile(manifest):
        table = []
        for ln in open(manifest).read().splitlines():
            ln = ln.strip()
            if not ln or ln.startswith("#"):
                continue
            rel, direction = ln.split("\t")[:2]
            table.append((rel, direction))
    rows, missing = [], []
    for rel, direction in table:
        path = os.path.join(ref, rel)
        if not os.path.isfile(path):
            missing.append(rel)
            continue
        src = open(path).read()
        pp = PP(root_dirs(ref))
        pp.obj["DEFINE_INTERFACE_HERE"] = "1"
        pp.obj.pop("DEFINE_SERVER_HERE", None)
        pp.obj["VOLATILE_POSITIONS_ARE_ABSOLUTE"] = "0"
        text = pp.process_file(path)
        iface = re.search(r"BEGIN_MERCURY_INTERFACE\s*\(\s*(\w+)",
                          strip_comments(src))
        iface = iface.group(1) if iface else ""
        for i, (name, style, param) in enumerate(messages_for(text)):
            f, ln = decl_site(ref, rel, name)
            rows.append({
                "interface": iface,
                "message": name,
                "id": i,
                "length_style": style,
                "length_param": param,
                "direction": direction,
                "source_file": rel,
                "decl_file": f or rel,
                "line": ln,
            })
    return rows, missing


if __name__ == "__main__":
    ref = sys.argv[1] if len(sys.argv) > 1 else os.path.join(
        os.environ.get("HOME", ""), "workspaces", "BigWorld",
        "programming", "bigworld")
    rows, missing = enumerate_interfaces(ref)
    cols = ["interface", "message", "id", "length_style", "length_param",
            "direction", "source_file", "decl_file", "line"]
    out = sys.stdout
    if len(sys.argv) > 2:
        out = open(sys.argv[2], "w")
    out.write("\t".join(cols) + "\n")
    for r in rows:
        out.write("\t".join(str(r[c]) for c in cols) + "\n")
    if out is not sys.stdout:
        out.close()
    if missing:
        sys.stderr.write("missing interface file(s): %s\n" % ", ".join(missing))
        sys.exit(2)