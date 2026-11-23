# Operations

Running the server once it exists: data on disk, load on the wire, and the
gates that decide whether a build is trustworthy.

## Sections

- [Database migration and recovery](/operations/database) — the four backends,
  the MySQL → PostgreSQL migration steps and the recovery procedure, tracked by
  **BW-09**.
- [Analytics: event contract and metrics](/operations/analytics) — the event
  envelope, the online/concurrency, duration, funnel, space-heat and custom
  aggregation metrics, and the job guarantees, tracked by **BW-26**.
- [Load testing with bots](/operations/load-testing) — the bot runtime,
  fixed-seed scenarios and the report contract, tracked by **BW-17**.
- [Failure matrix](/operations/failure-matrix) — which cluster faults the
  reference server recovers from automatically, per role, tracked by
  **BW-05-06**.
- [Testing and release](/operations/testing-and-release) — what is reproducible
  locally today, and the CI and release pipeline it builds towards (**BW-22**).

## Status warning

Every procedure here is a *target* written against `todo.md` ids: the storage
layer itself has not landed yet, and each page says which of its steps can be
run today (currently: none of the database ones).
