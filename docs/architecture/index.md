# Architecture

How the server is shaped: which processes exist, what each one owns, and what
Boundless must stay compatible with.

## Sections

- [Process topology](/architecture/process-topology) — one binary, one process
  per role, and what is wired today.
- [Mercury protocol](/architecture/mercury-protocol) — the reliable-UDP wire
  format, channels, retransmission, the message layer and what is actually
  compiled into the crate.
- [Entity pipeline](/architecture/entity-pipeline) — the `.def` contract, the
  CPython host, and the entity-creation link that does not exist yet.
- [Database model](/architecture/database-model) — the storage contract, the
  PostgreSQL backend, and the write path that has not landed.
- [Resource system](/architecture/resource-system) — BWResource path
  resolution and sections, plus the asset pipeline's infrastructure-only
  state.
- [Compatibility matrix](/architecture/compatibility-matrix) — the baseline
  commit, toolchains, platforms, backends and engine targets, each traced to a
  source file or a `todo.md` id.

## How these pages are maintained

Every claim here names the file it comes from. Where a piece of the topology is
not implemented, the page says so and names the `todo.md` id that tracks it, so
the topology can be read as both a description and a to-do map.
