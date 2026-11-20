# CLI reference

There is exactly one executable in the tree: `boundless-server`. There is no
clap or other subcommand framework — the surface below is everything.

## `boundless-server <role>`

```bash
cargo run -p boundless-server -- loginapp
```

`apps/server/src/main.rs` reads `args[1]` and passes it to
`server_runtime::app::run`. The role must be one of the eight in
[process topology](/architecture/process-topology):

```
baseapp baseappmgr cellapp cellappmgr dbapp dbappmgr loginapp reviver
```

### Behaviour

- Accepted role: prints `[loginapp] starting (boundless minimal entry)` to
  stderr and returns `0`.
- Unknown role: prints `boundless-server: unknown server role 'cellap'
  (expected one of: ...)` and exits `1`.
- Missing argument: same path with an empty role, so it is rejected as unknown
  (`empty_role_is_unknown` test).

::: warning Minimal lifecycle only
`run` returns after printing the startup line; it does not yet enter the
network loop. The full init chain (`bw.xml`, network interfaces, signal
handling) is tracked under BW-01-04, and `app.rs` says so in its module
docstring. Until then every role process exits by itself immediately after
starting.
:::

## Resource path flags

`BWResource::init_from_args` consumes two flags before any role logic runs
(`crates/resources/src/bwresource.rs`):

| Flag | Effect |
| --- | --- |
| `--res <path>`, `-r <path>` | Adds `<path>` to the resource search list |
| `-uid <n>` | Rewires the effective uid used for the personalised `bw_<username>.xml` and `~/.bwmachined.conf` lookup |

Non-matching arguments are returned to the caller as the *compressed tail* — the
Rust port of the reference's in-place `BWUtil::compressArgs`, which shrinks
`argv` so later parsers never see the flags.

## Config override flags

Any `+key value` pair anywhere on the command line is written into the front of
the `bw.xml` chain, overriding every file in it:

```bash
cargo run -p boundless-server -- \
  --res "$(pwd)/server" \
  loginapp \
  +Server/gameUpdateHertz 15 \
  +Server/production false
```

See [Configuration reference](/reference/configuration) for what each
key means and which defaults apply.

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Role started and was shut down cleanly |
| `1` | Unknown role, or any error in the startup pipeline |

The reference's internal `-1` from `doBWMainT` when config post-init validation
fails is a *config-level* signal, not the process exit code; the binary reports
it as `1`. See [process topology](/architecture/process-topology#exit-contract).

## Planned CLI surface

The target layout in `todo.md` adds separate entry points that do not exist yet:
`apps/tools/`, `apps/bots/`, `apps/editors/`, `apps/client/`. Nothing in this
tree accepts their arguments, and any command for them is documented here only
once the corresponding `todo.md` item lands.