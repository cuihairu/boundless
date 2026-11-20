# Operations

Running the server once it exists: data on disk, load on the wire, and the
gates that decide whether a build is trustworthy.

## Sections

- [Database migration and recovery](/operations/database) — the four backends,
  the MySQL → PostgreSQL migration steps and the recovery procedure, tracked by
  **BW-09**.
- [Load testing with bots](/operations/load-testing) — the bot runtime,
  fixed-seed scenarios and the report contract, tracked by **BW-17**.
- [Testing and release](/operations/testing-and-release) — what is reproducible
  locally today, and the CI and release pipeline it builds towards (**BW-22**).

## Status warning

Every procedure here is a *target* written against `todo.md` ids: the storage
layer itself has not landed yet, and each page says which of its steps can be
run today (currently: none of the database ones).
