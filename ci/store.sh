#!/usr/bin/env bash
# BW-22-02-02 — provision an isolated database and run the storage cases.
#
# Usage:
#   ci/store.sh                 # every backend this runner can provision
#   ci/store.sh mysql postgres  # just these
#   ci/store.sh self-test       # prove a failing case fails and is named
#
# Isolation is the point. A private server on a private socket with a private
# data directory, and a private database dropped when the cases finish. Nothing
# is left in whatever database the runner image happens to run, which is how a
# storage lane quietly ends up testing yesterday's residue.
#
# Cases are SQL files under tests/store. A case passes when every statement in
# it succeeds: the assertions are the statements themselves, so a backend that
# narrowed the id column fails on the insert rather than passing vacuously.

set -uo pipefail
# shellcheck source=ci/lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

MODE="${1:-all}"
CASES_DIR="$REPO_ROOT/tests/store"
SANDBOX="$(mktemp -d /tmp/bw4-store.XXXXXX)"

cleanup() {
    # The server is spawned inside a command substitution, so a pid captured
    # there dies with the subshell and the server leaks as an orphan. A pid
    # file inside the sandbox is the only thing that survives.
    local pidfile pid
    for pidfile in "$SANDBOX"/mysql/server.pid "$SANDBOX"/pg/server.pid; do
        [ -f "$pidfile" ] || continue
        pid="$(cat "$pidfile" 2>/dev/null)"
        [ -n "$pid" ] && kill "$pid" 2>/dev/null
    done
    sleep 1
    rm -rf "$SANDBOX"
}

# A provisioner that cannot even find its own tools exits 2. The caller turns
# that into a SKIP naming the missing piece, and a genuine failure (tools
# present, server will not start) into a FAIL. Distinguishing the two matters:
# a clean GitHub runner ships no database server, and that must not look like a
# broken lane.
MISSING_TOOLS=2
trap cleanup EXIT
# Bash does not run an EXIT trap for a fatal signal, and a cancelled job sends
# exactly that — the one moment a live database server would outlive the run.
# Turn the signals into an ordinary exit so cleanup still runs.
trap 'exit 143' TERM
trap 'exit 130' INT
trap 'exit 129' HUP

tsv_escape() { printf '%s' "$1" | tr '\t\n' '  '; }

record_path() { printf '%s/store/%s.tsv' "$CI_OUT" "$1"; }

# The shared cases plus the ones a backend brings of its own. A case is green
# only if every statement in it succeeded.
case_files() { # $1 backend $2 glob
    local backend="$1" glob="$2"
    [ -d "$CASES_DIR" ] || return 1
    {
        ls "$CASES_DIR/common/"$glob 2>/dev/null
        ls "$CASES_DIR/$backend/"$glob 2>/dev/null
    } 2>/dev/null | sort -u
    # The missing dialect directory is an ordinary outcome, not a failure:
    # with pipefail the failing ls would sink the whole pipeline.
    return 0
}

# Which ticket owns the substrate a backend needs. A SKIP has to point at the
# work that turns it on, not at the lane itself.
tracker_for() {
    case "$1" in
    xml | sqlite) printf 'BW-09-03' ;;
    *)            printf 'BW-09-02 and BW-09-06' ;;
    esac
}

which_first() {
    local b
    for b in "$@"; do
        if command -v "$b" >/dev/null 2>&1; then printf '%s' "$b"; return 0; fi
    done
    return 1
}

# --- postgresql ------------------------------------------------------------

# Ubuntu ships the server under /usr/lib/postgresql/<ver>/bin and does not
# symlink it into PATH, so PATH alone never finds it. CI_STORE_PG_BIN pins a
# directory when a runner has its own.
PG_BIN_DIR="${CI_STORE_PG_BIN:-}"

pg_bin_dir() {
    local d
    if [ -n "$PG_BIN_DIR" ] && [ -x "$PG_BIN_DIR/initdb" ]; then
        printf '%s' "$PG_BIN_DIR"
        return 0
    fi
    for d in /usr/lib/postgresql/*/bin /usr/pgsql-*/bin /usr/local/pgsql/bin; do
        [ -x "$d/initdb" ] && { printf '%s' "$d"; return 0; }
    done
    return 1
}

pg_path() {
    local b="$1" d
    command -v "$b" >/dev/null 2>&1 && { printf '%s' "$b"; return 0; }
    if d="$(pg_bin_dir)"; then
        [ -x "$d/$b" ] && { printf '%s' "$d/$b"; return 0; }
    fi
    return 1
}

pg_port() { printf '%s' "${CI_STORE_PG_PORT:-5433}"; }

# The locally provisioned server is trust-authenticated, so it takes no
# password; a workflow service container does. psql reads the password from the
# environment: -W is --password and -P is pager options, neither is usable here.
pg_password() {
    if [ -n "${CI_STORE_PG_PASSWORD:-}" ]; then
        PGPASSWORD="$CI_STORE_PG_PASSWORD"
        export PGPASSWORD
    fi
}

# Either a database the workflow already started as a service container, or one
# this runner builds itself. The handle printed is what every pg_* helper
# connects through: a socket directory locally, a hostname on a runner where
# the workflow supplies the server. Either way the caller gets a private
# database that is dropped before the lane ends.
provision_postgres() {
    if [ -n "${CI_STORE_PG_HOST:-}" ]; then
        printf '%s' "$CI_STORE_PG_HOST"
        return 0
    fi
    local missing="" b dir
    for b in initdb pg_ctl psql; do
        pg_path "$b" >/dev/null || missing="$missing $b"
    done
    if [ -n "$missing" ]; then
        echo "no PostgreSQL server on this runner (missing:$missing)" >&2
        return "$MISSING_TOOLS"
    fi
    dir="$SANDBOX/pg"
    mkdir -p "$dir/data" "$dir/run"
    if ! "$(pg_path initdb)" --no-instructions --auth=trust --username=boundless \
        -D "$dir/data" >"$SANDBOX/pg-init.log" 2>&1; then
        echo "initdb failed (see pg-init.log)" >&2
        return 1
    fi
    # -w start blocks until the postmaster answers, so no readiness poll is
    # needed afterwards; the postmaster is still running when pg_ctl returns.
    if ! "$(pg_path pg_ctl)" -D "$dir/data" \
        -o "-p 5433 -k $dir/run -c listen_addresses=''" \
        -l "$dir/err.log" -w start >/dev/null 2>&1; then
        echo "pg_ctl start failed (see err.log)" >&2
        return 1
    fi
    head -1 "$dir/data/postmaster.pid" >"$dir/server.pid"
    printf '%s' "$dir/run"
    return 0
}

pg_exec() { # $1 socket-dir-or-host $2 database $3 case
    pg_password
    "$(pg_path psql)" -h "$1" -p "$(pg_port)" -U boundless -d "$2" \
        -v ON_ERROR_STOP=1 -f "$3"
}

pg_drop_db() {
    local where="$1" db="$2"
    pg_password
    "$(pg_path psql)" -h "$where" -p "$(pg_port)" -U boundless -d postgres \
        -c "DROP DATABASE IF EXISTS \"$db\"" >/dev/null 2>&1
}

pg_db_exists() {
    local where="$1" db="$2"
    pg_password
    "$(pg_path psql)" -h "$where" -p "$(pg_port)" -U boundless -d postgres \
        -tAc "SELECT count(*) FROM pg_database WHERE datname='$db'" 2>/dev/null | grep -qx 1
}

pg_create_db() {
    pg_password
    "$(pg_path psql)" -h "$1" -p "$(pg_port)" -U boundless -d postgres \
        -c "CREATE DATABASE \"$2\"" >/dev/null 2>&1
}

mysql_install() { which_first mariadb-install-db mysql_install_db; }
mysql_server() { which_first mariadbd mysqld; }
mysql_cli() { which_first mysql mariadb; }

# --- mysql / mariadb -------------------------------------------------------

# Start a throwaway server on a private socket and echo that socket. socket-only
# on purpose: no TCP port is taken, so whatever listens on the image's 3306 is
# neither used nor blocked.
provision_mysql() {
    local dir="$SANDBOX/mysql"
    local sock="$dir/mysql.sock" data="$dir/data"
    mkdir -p "$dir/run"
    local installer server cli
    if ! cli="$(mysql_cli)"; then
        echo "no mysql or mariadb client on this runner" >&2
        return "$MISSING_TOOLS"
    fi
    # A service container the workflow started for this job. Nothing to
    # provision then, only to reach.
    if [ -n "${CI_STORE_MYSQL_HOST:-}" ]; then
        printf '%s' "$CI_STORE_MYSQL_HOST:${CI_STORE_MYSQL_PORT:-3306}"
        return 0
    fi
    if ! installer="$(mysql_install)"; then
        echo "no mariadb-install-db or mysql_install_db on this runner" >&2
        return "$MISSING_TOOLS"
    fi
    if ! server="$(mysql_server)"; then
        echo "no mariadbd or mysqld on this runner" >&2
        return "$MISSING_TOOLS"
    fi

    "$installer" --no-defaults --datadir="$data" \
        --auth-root-authentication-method=normal --user="$(id -un)" \
        >"$SANDBOX/mysql-init.log" 2>&1 || {
        echo "$installer could not initialise $data (see mysql-init.log)" >&2
        return 1
    }

    "$server" --no-defaults --datadir="$data" --socket="$sock" \
        --pid-file="$dir/mysql.pid" --skip-networking --user="$(id -un)" \
        --log-error="$dir/err.log" >/dev/null 2>&1 &
    echo "$!" >"$dir/server.pid"
    local _
    for _ in $(seq 1 60); do
        if "$cli" -u root --socket="$sock" -N -e 'SELECT 1' >/dev/null 2>&1; then
            printf '%s' "$sock"
            return 0
        fi
        sleep 0.5
    done
    echo "$server did not become ready (see err.log)" >&2
    return 1
}

# A handle is either a socket path this runner owns, or host:port when the
# workflow supplied a service container. Every helper below takes the handle and
# builds its own connection arguments from it, so both shapes go through one
# code path.
MYSQL_ARGS=()
mysql_args() {
    local handle="$1"
    MYSQL_ARGS=()
    if [[ "$handle" == *:* ]]; then
        MYSQL_ARGS+=(-h "${handle%%:*}" -P "${handle##*:}" \
            -u "${CI_STORE_MYSQL_USER:-root}")
        [ -n "${CI_STORE_MYSQL_PASSWORD:-}" ] && \
            MYSQL_ARGS+=("-p${CI_STORE_MYSQL_PASSWORD}")
    else
        MYSQL_ARGS+=(--socket="$handle" -uroot)
    fi
}

mysql_drop_db() {
    local sock="$1" db="$2"
    [ -n "$sock" ] || return 0
    mysql_args "$sock"
    "$(mysql_cli)" "${MYSQL_ARGS[@]}" -N \
        -e "DROP DATABASE IF EXISTS \`$db\`" >/dev/null 2>&1
}

mysql_db_exists() {
    local sock="$1" db="$2"
    mysql_args "$sock"
    "$(mysql_cli)" "${MYSQL_ARGS[@]}" -N -e 'SHOW DATABASES' 2>/dev/null | grep -qx "$db"
}

mysql_apply() { # $1 handle $2 database $3 case
    # -D, not -d: on MariaDB -d is --debug.
    mysql_args "$1"
    "$(mysql_cli)" "${MYSQL_ARGS[@]}" -D "$2" <"$3"
}

mysql_create_db() {
    mysql_args "$1"
    "$(mysql_cli)" "${MYSQL_ARGS[@]}" -N -e "CREATE DATABASE \`$2\`" >/dev/null 2>&1
}

# --- sqlite ---------------------------------------------------------------

# The reference secondary store reads its columns back with
# sqlite3_column_int64, so the i64 vocabulary applies here too. A private file
# is the whole server: nothing to start, and nothing left when it is removed.
provision_sqlite() {
    if ! command -v sqlite3 >/dev/null 2>&1; then
        echo "no sqlite3 on this runner" >&2
        return "$MISSING_TOOLS"
    fi
    local dir="$SANDBOX/sqlite"
    mkdir -p "$dir"
    printf '%s' "$dir"
}

sqlite_create_db() { : >"$1/$2"; }
sqlite_drop_db()  { rm -f "$1/$2"; }
sqlite_db_exists() { [ -f "$1/$2" ]; }

sqlite_apply() { # $1 dir $2 db $3 case
    # -bail: without it sqlite3 runs on past a failing statement and reports
    # success for a case that only half applied.
    sqlite3 -bail "$1/$2" <"$3"
}

# --- xml ------------------------------------------------------------------

# The reference XML backend writes an id out as a decimal attribute and reads it
# back, so what the lane checks is that the decimal representation of the i64
# domain round-trips. python3 is already a dependency of this workflow.
provision_xml() {
    if ! command -v python3 >/dev/null 2>&1; then
        echo "no python3 on this runner, so the xml id vocabulary cannot be checked" >&2
        return "$MISSING_TOOLS"
    fi
    local dir="$SANDBOX/xml"
    mkdir -p "$dir"
    printf '%s' "$dir"
}

xml_create_db() { mkdir -p "$1/$2"; }
xml_drop_db()  { rm -rf "${1:?handle}/${2:?db}"; }
xml_db_exists() { [ -d "$1/$2" ]; }

xml_apply() { # $1 dir $2 db $3 case
    # The case is accepted only when every id fits the i64 domain and at least
    # one is present — a file with no ids would otherwise pass vacuously.
    python3 -c '
import os, sys, xml.etree.ElementTree as ET
I64_MIN, I64_MAX = -(1 << 63), (1 << 63) - 1
outdir, path = sys.argv[1], sys.argv[2]
try:
    root = ET.parse(path).getroot()
except ET.ParseError as e:
    print("ERROR: not well-formed xml: %s" % e); sys.exit(1)
ids = []
for el in root.iter():
    for k, v in el.attrib.items():
        if k.lower() in ("id", "entityid"):
            try: ids.append(int(v, 10))
            except ValueError:
                print("ERROR: id %r is not a decimal integer" % v); sys.exit(1)
if not ids:
    print("ERROR: no id attributes, so this case would pass vacuously"); sys.exit(1)
bad = [i for i in ids if not (I64_MIN <= i <= I64_MAX)]
if bad:
    print("ERROR: %d id(s) outside the i64 domain, e.g. %d" % (len(bad), bad[0])); sys.exit(1)
name = os.path.basename(path)
name = name[:-len(".xml")] if name.endswith(".xml") else name
with open(os.path.join(outdir, name + ".ids"), "w") as f:
    for i in ids:
        f.write("%d\n" % i)
print("ok %d id(s), min=%d max=%d" % (len(ids), min(ids), max(ids)))
' "$1/$2" "$3"
}

# --- the lane --------------------------------------------------------------

run_backend() { # $1..$6 backend + its helpers, $7 the case glob
    local backend="$1" provision="$2" create_db="$3" drop_db="$4" apply="$5" db_exists="$6"
    local glob="$7"
    local rec db cases ok=0 total=0 failed="" residue where err rc

    ci_step "$backend"
    rec="$(record_path "$backend")"
    mkdir -p "$CI_OUT/store" "$CI_LOG_DIR"

    cases="$(case_files "$backend" "$glob" || true)"
    if [ -z "$cases" ]; then
        ci_skip "store-$backend" "no cases under $CASES_DIR (common/ or $backend/); this backend is tracked by $(tracker_for "$backend")"
        return 0
    fi

    err="$SANDBOX/$backend-provision.err"
    where="$("$provision" 2>"$err")"
    rc=$?
    if [ "$rc" -eq "$MISSING_TOOLS" ]; then
        ci_skip "store-$backend" "$(tsv_escape "$(cat "$err" 2>/dev/null)")"
        return 0
    fi
    if [ "$rc" -ne 0 ]; then
        ci_fail "store-$backend" "cannot provision an isolated $backend: $(tsv_escape "$(cat "$err" 2>/dev/null)")"
        return 0
    fi

    db="boundless_ci_$$"
    "$create_db" "$where" "$db" 2>/dev/null

    {
        printf 'backend\t%s\n' "$backend"
        printf 'provisioned\t%s\n' "$(tsv_escape "$where")"
        printf 'database\t%s\n' "$db"
        printf 'case\tresult\tfirst_error\n'
    } >"$rec"

    local file name out rc
    while IFS= read -r file; do
        [ -n "$file" ] || continue
        name="$(basename "$file")"
        name="${name%.*}"
        total=$((total + 1))
        out="$("$apply" "$where" "$db" "$file" 2>&1)"
        rc=$?
        if [ "$rc" -eq 0 ]; then
            ok=$((ok + 1))
            printf '%s\tok\t-\n' "$name" >>"$rec"
        else
            failed="$failed $name"
            printf '%s\tFAIL\t%s\n' "$name" "$(tsv_escape "$(printf '%s' "$out" | grep -m1 -iE 'error' | cut -c1-140)")" >>"$rec"
        fi
    done <<<"$cases"

    "$drop_db" "$where" "$db" 2>/dev/null
    if "$db_exists" "$where" "$db" 2>/dev/null; then
        residue=no
    else
        residue=yes
    fi
    printf 'database_dropped\t%s\n' "$residue" >>"$rec"
    cat "$rec" >>"$CI_LOG_DIR/store-$backend.log"

    if [ "$ok" -eq "$total" ] && [ "$residue" = yes ]; then
        ci_pass "store-$backend" "$ok/$total cases ok, isolated $backend left nothing behind: $(basename "$rec")"
    else
        ci_fail "store-$backend" "${failed:-unknown} ($ok/$total cases ok, dropped=$residue)"
        return 0
    fi
    ci_note "record: $rec"
}

run() {
    mkdir -p "$CI_OUT" "$CI_LOG_DIR"
    [ -n "${GITHUB_ENV:-}" ] && ci_out_env >>"$GITHUB_ENV"

    local wanted="${*:-mysql postgres sqlite xml}" b
    for b in $wanted; do
        case "$b" in
        mysql | mariadb)
            run_backend mysql provision_mysql mysql_create_db mysql_drop_db mysql_apply mysql_db_exists '*.sql'
            ;;
        postgres | postgresql)
            run_backend postgres provision_postgres pg_create_db pg_drop_db pg_exec pg_db_exists '*.sql'
            ;;
        sqlite)
            run_backend sqlite provision_sqlite sqlite_create_db sqlite_drop_db sqlite_apply sqlite_db_exists '*.sql'
            ;;
        xml)
            run_backend xml provision_xml xml_create_db xml_drop_db xml_apply xml_db_exists '*.xml'
            ;;
        *)
            ci_skip "store-$b" "unknown backend '$b' (mysql, postgres, sqlite, xml)"
            ;;
        esac
    done
}

self_test() {
    local fails=0 cli sock db bad out rc
    ci_step "self-test: a narrowed id column fails the case"
    if ! cli="$(mysql_cli)"; then
        ci_skip selftest-store "no MySQL client on this runner — the isolation self-test needs one"
        return 0
    fi
    sock="$(provision_mysql 2>"$SANDBOX/selftest-provision.err")"
    rc=$?
    if [ "$rc" -eq "$MISSING_TOOLS" ]; then
        ci_skip selftest-store "$(tsv_escape "$(cat "$SANDBOX/selftest-provision.err" 2>/dev/null)")"
        return 0
    fi
    if [ "$rc" -ne 0 ]; then
        ci_fail selftest-store "cannot provision an isolated MySQL here: $(tsv_escape "$(cat "$SANDBOX/selftest-provision.err" 2>/dev/null)")"
        return 0
    fi
    db="boundless_ci_selftest_$$"
    mysql_create_db "$sock" "$db"
    # Prove the database exists before planting the defect. Without this check a
    # CREATE DATABASE that silently failed would make the planted case fail for
    # the wrong reason ("unknown database"), and this self-test would go green
    # without having tested anything at all.
    if ! mysql_db_exists "$sock" "$db"; then
        ci_fail selftest-store "could not create the private database $db, so nothing is being tested"
        return 1
    fi

    # The id column is declared 32 bits wide: the i64 boundary inserts cannot
    # fit, so the case must fail rather than pass silently.
    bad="$SANDBOX/bad.sql"
    printf 'CREATE TABLE bw_id_contract (id INT NOT NULL PRIMARY KEY);\nINSERT INTO bw_id_contract VALUES (2147483648);\n' >"$bad"
    out="$(mysql_apply "$sock" "$db" "$bad" 2>&1)"
    rc=$?
    # The refusal must be about the value not fitting, not about anything else.
    # 1264 is MySQL's ER_TRUNCATED_WRONG_VALUE; MariaDB and MySQL word it the
    # same way. A different refusal would mean this self-test is testing the
    # wrong thing and must not pass.
    if [ "$rc" -ne 0 ] && printf '%s' "$out" | grep -qiE 'out of range|out-of-range|1264'; then
        ci_pass selftest-store "a narrowed id column fails the case: $(printf '%s' "$out" | grep -im1 -iE 'out of range|1264' | cut -c1-90)"
    else
        ci_fail selftest-store "the planted defect did not refuse on range (rc=$rc): $(printf '%s' "$out" | grep -im1 -iE 'error|range' | cut -c1-90)"
        fails=$((fails + 1))
    fi

    ci_step "self-test: the private database is really dropped"
    mysql_drop_db "$sock" "$db"
    if ! mysql_db_exists "$sock" "$db"; then
        ci_pass selftest-store-drop "no residue after DROP DATABASE"
    else
        ci_fail selftest-store-drop "the private database is still there"
        fails=$((fails + 1))
    fi

    if [ "$fails" -gt 0 ]; then
        ci_note "$fails self-test(s) did not behave"
        return 1
    fi
    ci_note "all self-tests behaved as expected"
    return 0
}

main() {
    if [ "$MODE" = self-test ]; then
        self_test
        return $?
    fi
    run "${@:-mysql postgres sqlite xml}"
}

main "$@"
LANE_RC=$?

set +e
ci_finalize
FINAL_RC=$?
set -e

[ "$LANE_RC" -ne 0 ] && exit "$LANE_RC"
exit "$FINAL_RC"