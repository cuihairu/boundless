# Testing and release

This page lists the steps that can be reproduced locally today, and the CI and
release pipeline they are building towards.

## Reproducible today

There is no `.github/` directory, no `ci/` directory, no Makefile and no
justfile in this repository. Everything below runs in a plain checkout.

### Toolchain

```bash
cat rust-toolchain.toml        # channel = "1.100.0"
rustup show active-toolchain
```

The toolchain is pinned in `rust-toolchain.toml` so that a local checkout and
CI resolve the same compiler (tracked as BW-00-03-02). Do not bypass it with a
per-directory override.

### Build and test

```bash
cargo build -p boundless-server      # the only binary
cargo test --workspace               # every inline unit test
cargo test -p server-runtime         # config chain, tick, timers, signals, shutdown
cargo test -p foundation -p platform -p resources -p mercury -p entitydef
```

All tests are inline `#[cfg(test)]` modules; there is no `tests/` directory and
no integration harness yet. The internal test framework (fixtures, virtual
time, multi-process topologies, fault injection) is BW-18.

### Documentation gate

The documentation site is itself a build gate, and it is the one part of the
planned CI that is fully reproducible right now:

```bash
cd docs
pnpm install
pnpm run check:links    # fails on any dead internal link
pnpm run build          # fails on dead links too (ignoreDeadLinks: false)
pnpm run dev            # preview at http://localhost:5173
pnpm run preview        # serve the production bundle
```

`check:links` walks every markdown file under `docs/`, skips `.vitepress/`, and
resolves each link the way VitePress would: absolute destinations are rooted at
the docs root, page targets may be `.md`, `index.md` or `.html`, and asset
targets must exist under `docs/public/`. Exit code `0` means no dead links, `1`
means at least one.

Both gates run from `docs/`, which is where `docs/package.json` lives, so CI
can invoke them as `pnpm --dir docs run build`.

## Planned CI (BW-22)

Scripts go in `ci/`, and workflows invoke the same entry points so local and CI
cannot drift.

| Group | Contents |
| --- | --- |
| BW-22-01 build and static checks | pinned toolchain and lockfile, Rust format/lint/build, Python checks, TypeScript checks, path-conditioned jobs |
| BW-22-02 test matrix | three platform runners, real MySQL and PostgreSQL, XML and SQLite, compatibility diffs, SDK and plugin tests |
| BW-22-03 load and regression | short PR smoke, scheduled capacity and soak, manual fault scenarios, comparable reports |
| BW-22-04 release pipeline | three-platform installers, the documentation site and its versioned index, SDK packages, the VSIX, metadata and hash checks |

Two properties of this design are worth keeping:

- **Path conditions.** Documentation changes trigger only the site checks; code
  changes do not. That is BW-22-01-05, and it is why the docs site has its own
  build entry rather than piggy-backing on the Rust build.
- **Every job locally reproducible.** BW-22-02-06 requires archived coverage,
  service logs and failure fixtures. A CI step you cannot rerun on a laptop is a
  CI step you cannot debug.

## Load regression

Short scenarios run on pull requests; capacity and long-soak scenarios run on a
schedule or manually. Reports must record version, hardware, topology, database
backend and scenario version to be comparable across runs (BW-22-03-04). See
[Load testing with bots](/operations/load-testing).

## Release

There is no release pipeline in this tree and no release has been produced from
it. Version numbers are assigned by hand, not by automation: this site does not
bump a version, cut a tag or publish an artifact.

The artifact set the pipeline is building towards (BW-22-04) is:

- three-platform binaries and installers, smoke-tested by installing and
  starting in a clean environment
- the documentation site plus a version-matched documentation index
- SDK packages per engine, importable into an empty project
- the VS Code extension as a VSIX, installable in a clean host
- metadata and hashes, with tampered artifacts detected

Until BW-14 (FantasyDemo and distribution) and BW-22-04 land, the only
shippable artifact is the binary produced by `cargo build -p boundless-server`.