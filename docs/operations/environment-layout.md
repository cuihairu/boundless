# Environment layout

Every engine process needs four kinds of paths, and each is scoped so two
environments on the same machine can run side by side without overwriting each
other. This page is the BW-00-03-04 record: which path is which, how it is
resolved, and the guarantee that makes co-located environments safe.

## The four paths

| Path class | Resolved from | Scope |
| --- | --- | --- |
| Resources | `--res` / `-r` flags, then `BW_RES_PATH`, then the first usable line of `~/.bwmachined.conf` (tokenised on `:`); cache size from `BW_CACHE_SIZE` | per invocation (flag/env) or per user (machined.conf) |
| Config | `server/bw_<username>.xml` (the passwd entry of the effective uid, which `-uid` / `UID` rewires) → `server/bw.xml` → `<parentFile>` chain | per user |
| Reference artifacts | the upstream BigWorld tree, outside this repository and read-only; the committed fixtures live under `docs/review/reference/` | shared, never written |
| Test paths and outputs | `tests/` (fixtures: `parity`, `store`, `python`); results under `ci-out/` with the `CI_OUT` override (`CI_LOG_DIR=$CI_OUT/logs`, `CI_MANIFEST=$CI_OUT/manifest.tsv`) | per invocation / per CI run |

## Resource paths

`BWResource` in `crates/resources/src/bwresource.rs` resolves the resource tree
in this order, and drops a `+key` in the flag position because the loop runs to
`argc - 1`:

1. `--res` / `-r <path>` command line arguments
2. the deprecated `BW_RES_PATH` environment variable
3. the first usable line of `~/.bwmachined.conf` for `getUserId()`'s user

Paths are tokenised on `:` (`BW_RES_PATH_SEPARATOR` on unix), so an environment
can pass several independent roots as one list. See [Configuration
reference](/reference/configuration#resource-paths).

## Config chain

The chain is the BigWorld `bw.xml` family (see [Configuration
reference](/reference/configuration#the-chain)):

1. a personalised `server/bw_<username>.xml` starts the chain when it exists;
   `<username>` is the passwd entry of the effective uid, which the `-uid` flag
   and the `UID` environment variable rewire
2. when no personalised file exists, the chain starts at `server/bw.xml`
3. every file may name its parent with `<parentFile>`; the chain ends at an
   empty value or an unopenable file

## Two environments on one machine

The separation is the per-user filename. A distinct `UID` (or `-uid`) resolves
to a distinct passwd username, so two environments pick distinct front files
even when they share one resource root.

A command-line write-through (`+key value`) updates the writing environment's
front section only: `BwConfig::process_command_line` replaces that section and
re-adds it to the resource cache keyed by the environment's own filename
(`BwResource::readd_section`), so environment A's write never appears in
environment B's chain. Nothing on disk is overwritten — the write-through is
cache-scoped, and the shared `server/bw.xml` and the other environment's file
keep their bytes.

Proven by `config::tests::two_environments_on_one_machine_do_not_overwrite` in
`crates/server-runtime/src/config.rs`, which puts alice and bob on one shared
resource root, has each resolve its own chain, writes through alice's command
line, and asserts bob still reads his value while all three files keep their
original bytes.
