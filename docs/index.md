# Boundless

Boundless is a Rust re-implementation of the BigWorld MMO game server. The
workspace targets the BigWorld revision whose source baseline is recorded in
`todo.md`, and every design decision is traced back to that baseline or to an
explicit new design.

This site documents how to build, run and operate the server. The [Guide](/guide/)
starts here.

## Where to start

| You want to... | Read |
| --- | --- |
| Build and install on Linux, Windows or macOS | [Installing on three platforms](/guide/install) |
| Log in and enter a world with the FantasyDemo | [FantasyDemo quick start](/guide/fantasy-demo) |
| Start and stop one server role today | [Running a standalone server](/guide/standalone-server) |
| Write your own entity scripts | [Entity scripting](/guide/entity-authoring) |
| Connect a client or an engine | [Client SDK](/guide/client-sdk) and [Engine integration](/guide/engine-integration) |
| See which processes exist and what is wired | [Process topology](/architecture/process-topology) |
| Check a version, platform or engine target | [Compatibility matrix](/architecture/compatibility-matrix) |
| Look up a config key or a CLI flag | [Configuration reference](/reference/configuration) and [CLI reference](/reference/cli) |
| See what each crate exports | [Script and SDK API](/reference/api) |

## Repository layout

```
apps/
  server/           # the server role binary
crates/
  foundation/       # shared ids and primitives
  platform/         # platform interface (clock, poll, signals)
  resources/        # BWResource resource paths
  mercury/          # Mercury reliable-UDP packet framing
  contracts/        # shared message and data contracts
  entitydef/        # entity definition contracts
  python-host/      # embedded Python host
  server-runtime/   # process bootstrap, config, tick, timers
docs/
  .vitepress/       # this site's configuration
  guide/            # installation, quick start, deployment
  architecture/     # modules, protocol and process topology
  reference/        # configuration, script API, SDK and CLI
  operations/       # database, load testing, testing and release
  public/           # public static resources
```

The server is a single binary that selects its role from the first positional
argument: `boundless-server loginapp`. There are no per-role binaries and no
clap or subcommand framework.

## Current implementation status

The working tree is early. This site records what the source actually does today
and, where a page describes a target that is not implemented yet, names the
`todo.md` tracking id. Nothing here is presented as working when it is not.

| Area | State in the working tree | Tracked by |
| --- | --- | --- |
| Server binary and role set | Implemented, minimal lifecycle | `apps/server/src/main.rs` |
| `bw.xml` configuration chain | Implemented | `crates/server-runtime/src/config.rs` |
| Mercury packet framing | Implemented | `crates/mercury/src/packet.rs` |
| PostgreSQL persistence | Not implemented | `todo.md` BW-09-06 |
| FantasyDemo examples | Not implemented | `todo.md` BW-14 |
| Bots load testing | Not implemented | `todo.md` BW-17 |
| Client SDK and engine bindings | Not implemented | `todo.md` BW-21 |

::: warning No logo or design assets
The repository contains no logo, favicon or other design asset, and this site
deliberately ships none. VitePress uses its default theme chrome. When a logo is
authored, add it under `docs/public/` and reference it from
`.vitepress/config.mjs` rather than substituting a placeholder image.
:::

## Building this site

```bash
cd docs
pnpm install
pnpm run dev            # preview at http://localhost:5173
pnpm run build          # production bundle into .vitepress/dist
pnpm run preview        # serve the production bundle
pnpm run check:links    # fail on any dead internal link
```

The build is a real gate: `ignoreDeadLinks: false` in `.vitepress/config.mjs`
makes a broken internal link fail `pnpm run build`, and `check:links` is the
second gate behind it. Both run from `docs/`, where `docs/package.json` lives,
so CI invokes them as `pnpm --dir docs run build`.

Build output is `.vitepress/dist/`, which is ignored by `docs/.gitignore`. The
site is a private-repo artifact only: it is built and previewed, never published
to GitHub Pages.