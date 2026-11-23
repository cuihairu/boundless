#!/usr/bin/env python3
"""Collect resource, entity and database fixtures from the reference stack.

For BW-00-04-03 the three fixtures are:

  * resource fixtures - the files that define this scenario's data surface
    (the DB seed, the entity registration, the entity defs, the entity scripts
    and the config that actually ran), each with its path and content hash;
  * entity fixtures - every entity type the scenario uses, parsed out of its
    .def (properties, flags, defaults, methods), joined to the type id the
    database knows it under, plus the runtime entity one real login created;
  * database fixtures - the schema the dbapp laid down and the rows still in
    the database, per table, with the entity-def checksum that ties the
    schema back to the .def fixtures.

Every fixture records the reference commit it came from and the inputs it was
built from, so a diff of two stacks can say which side produced which bytes.

The scan is read-only: the cluster and MariaDB are observed, never driven, so
it works against a cluster another session owns (mode `live`) as well as one
brought up here.

Usage: entity_db_fixture_scan.py BIGWORLD_ROOT LOGIN_JSON OUTPUT_JSON
"""
import hashlib
import json
import os
import subprocess
import sys
import time
import xml.etree.ElementTree as ET

DB_HOST, DB_USER, DB_PASS, DB_NAME = "127.0.0.1", "bigworld", "bigworld", "bigworld"
EX = "programming/bigworld/examples/client_integration/python/simple"
DEF_DIR = EX + "/res/scripts/entity_defs"

# role -> (path, note) for the files this scenario reads.
RESOURCE_FILES = [
    (EX + "/res/scripts/db.xml", "db-seed",
     "种子：登录映射与实体种子行（dbapp 首次装载的输入）"),
    (EX + "/res/scripts/entities.xml", "registration",
     "实体注册表（哪些类型对客户端可见）"),
    (DEF_DIR + "/alias.xml", "alias", "数据类型别名表"),
    (EX + "/res/scripts/entity_defs/Account.def", "entity-def", "实体定义"),
    (EX + "/res/scripts/entity_defs/ClientAvatar.def", "entity-def", "实体定义"),
    (EX + "/res/scripts/entity_defs/Guard.def", "entity-def", "实体定义"),
    (EX + "/res/scripts/entity_defs/Space.def", "entity-def", "实体定义"),
    (EX + "/res/scripts/entity_defs/ClientOnly.def", "entity-def", "实体定义"),
    (EX + "/res/scripts/base/Account.py", "entity-script", "base 端脚本"),
    (EX + "/res/scripts/base/ClientAvatar.py", "entity-script", "base 端脚本"),
    (EX + "/res/scripts/base/Guard.py", "entity-script", "base 端脚本"),
    (EX + "/res/scripts/base/Space.py", "entity-script", "base 端脚本"),
    (EX + "/res/scripts/base/egclient4.py", "entity-script", "base 端脚本"),
    (EX + "/res/scripts/cell/ClientAvatar.py", "entity-script", "cell 端脚本"),
    (EX + "/res/scripts/cell/Guard.py", "entity-script", "cell 端脚本"),
    (EX + "/res/scripts/cell/Space.py", "entity-script", "cell 端脚本"),
    (EX + "/res/scripts/client/Account.py", "entity-script", "客户端脚本"),
    (EX + "/res/scripts/client/ClientAvatar.py", "entity-script", "客户端脚本"),
    (EX + "/res/scripts/client/ClientOnly.py", "entity-script", "客户端脚本"),
    (EX + "/res/scripts/client/Guard.py", "entity-script", "客户端脚本"),
    (EX + "/res/scripts/client/Space.py", "entity-script", "客户端脚本"),
    (EX + "/res/scripts/common/TestDataType.py", "entity-script",
     "prop5 的用户类型实现"),
    (EX + "/res/server/bw.xml", "config", "实体树配置"),
    ("scripts/cluster-smoke/res/server/bw_deep.xml", "config",
     "实际运行配置（deep-chain 覆盖层）"),
]

BOOKKEEPING = [
    ("bigworldLogOnMapping", "version", "schema 版本"),
    ("bigworldLogOnMapping", "isPasswordHashed", "密码哈希开关"),
    ("bigworldGameTime", "time", "世界时钟"),
    ("bigworldNewID", "id", "实体 ID 计数器"),
    ("bigworldEntityDefsChecksum", "checksum", "实体定义校验和"),
]
EMPTY_TABLES = ["bigworldLogOns", "bigworldInfo", "bigworldSpaces",
               "bigworldSpaceData", "bigworldSecondaryDatabases",
               "bigworldUsedIDs"]


def git_short_head(root):
    return subprocess.run(["git", "-C", root, "rev-parse", "--short", "HEAD"],
                          capture_output=True, text=True).stdout.strip()


def _mysql(query, headers):
    args = ["mysql", "-h", DB_HOST, "-u", DB_USER, "-p" + DB_PASS, DB_NAME,
            "-B", "--batch"]
    if not headers:
        args += ["-N"]
    return subprocess.run(args + ["-e", query],
                          capture_output=True, text=True).stdout


def sql(query):
    """Run one read-only query; return the raw batch output (no headers)."""
    return _mysql(query, headers=False)


def rows(query):
    """Run one read-only query; return a list of row dicts (headers on)."""
    lines = [l for l in _mysql(query, headers=True).splitlines() if l.strip()]
    if len(lines) < 2:
        return []
    cols = lines[0].split("\t")
    return [dict(zip(cols, l.split("\t"))) for l in lines[1:]]


def parse_def(path):
    """One .def into {volatile, properties, client_methods, ...}."""
    root = ET.parse(path).getroot()
    def text_of(parent, tag):
        el = parent.find(tag) if parent is not None else None
        return (el.text or "").strip() if el is not None else None
    volatile_el = root.find("Volatile")
    volatile = [el.tag for el in volatile_el] if volatile_el is not None else []
    props = []
    props_el = root.find("Properties")
    if props_el is not None:
        for el in list(props_el):
            typ_el = el.find("Type")
            typ = (typ_el.text or "").strip() if typ_el is not None else None
            by = el.find("Type/implementedBy")
            props.append({
                "name": el.tag,
                "type": typ,
                "implemented_by": (by.text or "").strip()
                if by is not None else None,
                "flags": (text_of(el, "Flags") or "").split(),
                "default": text_of(el, "Default"),
                "persistent": text_of(el, "Persistent"),
                "editable": text_of(el, "Editable"),
                "identifier": text_of(el, "Identifier"),
            })
    def methods(tag):
        el = root.find(tag)
        out = []
        if el is not None:
            for m in list(el):
                out.append({
                    "name": m.tag,
                    # <Arg>STRING</Arg> carries its type as text; markers such
                    # as <Exposed/> have none, so keep the tag either way.
                    "params": [{"tag": p.tag,
                                "text": (p.text or "").strip() or None}
                               for p in list(m)],
                })
        return out
    return {
        "volatile": volatile, "properties": props,
        "client_methods": methods("ClientMethods"),
        "cell_methods": methods("CellMethods"),
        "base_methods": methods("BaseMethods"),
    }


def main():
    if len(sys.argv) < 4:
        print(__doc__)
        return 1
    root, login_json, out_json = sys.argv[1:4]
    with open(login_json, encoding="utf-8") as fh:
        login = json.load(fh)

    commit = git_short_head(root)
    mode = "live" if subprocess.run(
        ["pgrep", "-x", "bwmachined2"], capture_output=True,
        text=True).stdout.strip() else "absent"

    # ---- resource fixtures -------------------------------------------------
    resources = []
    for rel, role, note in RESOURCE_FILES:
        path = os.path.join(root, rel)
        data = open(path, "rb").read()
        resources.append({
            "path": rel, "role": role, "note": note,
            "bytes": len(data),
            "sha256": hashlib.sha256(data).hexdigest(),
        })

    # ---- entity fixtures ---------------------------------------------------
    defs = []
    for el in sorted(os.listdir(os.path.join(root, DEF_DIR))):
        if not el.endswith(".def"):
            continue
        rel = DEF_DIR + "/" + el
        p = parse_def(os.path.join(root, DEF_DIR, el))
        p.update({"type": el[:-4], "path": rel,
                  "sha256": hashlib.sha256(
                      open(os.path.join(root, rel), "rb").read()).hexdigest(),
                  "bytes": os.path.getsize(os.path.join(root, rel))})
        defs.append(p)

    # ---- database fixtures -------------------------------------------------
    def snapshot_db():
        # The type ids the database knows each entity type under ride along:
        # they are how an entity fixture joins to its database table.
        db_types = {r["name"]: r
                    for r in rows("SELECT * FROM bigworldEntityTypes")}
        names = [r["Tables_in_%s" % DB_NAME] for r in rows("SHOW TABLES")]
        tables = []
        for name in names:
            # SHOW CREATE TABLE is the authoritative DDL as MariaDB wrote it:
            # it records the column types the dbapp derived from the .defs.
            d = rows("SHOW CREATE TABLE `%s`" % name)[0]
            tables.append({
                "name": name,
                "create_sql": d["Create Table"].replace("\\n", "\n"),
                "rows": int(sql("SELECT COUNT(*) FROM `%s`" % name).strip() or 0),
            })
        bookkeeping = {}
        for table, col, note in BOOKKEEPING:
            v = sql("SELECT `%s` FROM `%s`" % (col, table)).strip()
            bookkeeping[table + "." + col] = {"value": v, "note": note}
        counts = {tb: int(sql("SELECT COUNT(*) FROM `%s`" % tb).strip() or 0)
                 for tb in EMPTY_TABLES}
        return {"tables": tables, "bookkeeping": bookkeeping,
                "row_counts": counts, "entity_types": db_types}

    # The database is shared with whoever owns the live cluster, so the read
    # can race another session's re-provision. Retry until two consecutive
    # snapshots agree (the schema is what a diff consumes), else keep the last
    # and mark the capture unstable.
    db_fixture = snapshot_db()
    stable, attempts = False, 1
    for _ in range(3):
        again = snapshot_db()
        attempts += 1
        if again == db_fixture:
            stable = True
            break
        db_fixture = again
    db_fixture["stable"] = stable
    db_fixture["attempts"] = attempts

    # Join the (stabilised) database type ids onto the parsed defs, so each
    # entity fixture carries both its file identity and its database identity.
    db_types = db_fixture.pop("entity_types")
    for p in defs:
        t = db_types.get(p["type"])
        p["db_type_id"] = t["typeID"] if t else None
        p["db_bigworld_id"] = t["bigworldID"] if t else None

    data = {
        "source_commit": commit,
        "captured_at": time.strftime("%Y-%m-%d %H:%M:%S %z"),
        "mode": mode,
        "inputs": {
            "res_path": login.get("res_path"),
            "login_command": login.get("command"),
            "database": "%s@%s on %s" % (DB_NAME, DB_USER, DB_HOST),
            "db_server": sql("SELECT VERSION()").strip(),
        },
        "runtime_entity": {
            "class": "ClientAvatar",
            "proxy_id": login.get("server_callbacks", {}).get("proxy_id"),
            "client_source_port": login.get("client_source_port"),
            "captured_at": login.get("captured_at"),
        },
        "resources": resources,
        "entities": defs,
        "database": db_fixture,
    }
    with open(out_json, "w", encoding="utf-8") as fh:
        json.dump(data, fh, indent=1, sort_keys=True)

    print("commit %s  mode=%s  db %s" % (commit, mode, data["inputs"]["db_server"]))
    print("resources %d   entities %d   tables %d (stable=%s after %d reads)" % (
        len(resources), len(defs), len(db_fixture["tables"]),
        db_fixture["stable"], db_fixture["attempts"]))
    for p in defs:
        print("  %-12s props %-2d typeID %s" % (
            p["type"], len(p["properties"]), p["db_type_id"]))
    for t in db_fixture["tables"]:
        print("  %-45s rows %d" % (t["name"], t["rows"]))
    return 0


if __name__ == "__main__":
    sys.exit(main())
