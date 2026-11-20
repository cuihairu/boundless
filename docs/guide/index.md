# Guide

Practical, command-level documentation: getting a build, getting a login in, and
getting a world running.

## How honest this guide is

Every page states which parts are implemented in the working tree and which
describe a target. The working tree is early: the server binary, the role set,
the `bw.xml` chain, Mercury packet framing and the entity type id space exist;
the login protocol, the Python host, FantasyDemo resources and the client SDK do
not yet. Pages that depend on the latter quote the reference revision's scripts
and name the `todo.md` id that lands them, so a reader can tell target from
reality without checking the source.

## Verifying a page's claims

Two commands are enough for most claims in this section:

```bash
cargo build -p boundless-server        # the binary and its role set
cargo test -p server-runtime            # config chain, tick, timers, signals
```