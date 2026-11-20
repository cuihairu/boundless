# Configuration reference

Server configuration is read from a **BigWorld `bw.xml` chain** — not TOML, JSON
or YAML. The port lives in `crates/server-runtime/src/config.rs`, which follows
`lib/server/bwconfig.cpp` line for line, including its quirks.

## The chain

1. A personalised `server/bw_<username>.xml` starts the chain when it exists.
   `<username>` is the passwd entry of the *effective* uid, which the `-uid`
   command line flag rewires.
2. When no personalised file exists, the chain starts at `server/bw.xml`.
3. Every file may name its parent with `<parentFile>`.
4. The chain ends at an empty `parentFile`, or at a file that cannot be opened.
   An unopenable file also records `badFilename`, which makes the config "bad".
5. Lookups scan the chain **front to back** and take the first hit, so the
   most-derived file wins and parents only supply what children leave unset.

::: tip Reference quirk kept on purpose
`BIGWORLD_MAIN` in the reference *discards* both the `BWResource::init` and
`BWConfig::init` return values: a missing config does not stop a server app, it
runs on the option defaults. Only `isBad()`-reporting tools such as
`consolidate_dbs` treat it as an early-out.
:::

## Command line overrides

`+key value` pairs are written into the **front** section, overriding the entire
chain (`BwConfig::process_command_line`). `writeString` creates the path
automatically, so a `+Server/gameUpdateHertz 15` pair introduces the path even
if no file in the chain defines it.

Two behaviours match the reference exactly:

- A trailing `+key` with no value argument is ignored (the reference scans to
  `argc - 1`).
- With **no config sections at all**, command line processing fails and prints
  `No configuration section.`

## `ServerAppConfig`

`crates/server-runtime/src/server_app.rs` maps the `BW_OPTION_RO` statics. Each
missing option logs a per-option warning and keeps its default
(`ServerAppOptionIniterT::init`).

| XML path | Field | Default |
| --- | --- | --- |
| `gameUpdateHertz` | `update_hertz` | `10` |
| `personality` | `personality` | `BWPersonality` |
| `serverMode` | `server_mode` | `standalone` |
| `production` | `is_production` | `true` |
| `timeSyncPeriod` | `time_sync_period` | `60` |
| — (derived) | `time_sync_period_in_ticks` | derived |
| `useDefaultSpace` | `use_default_space` | `false` |
| `maxOpenFileDescriptors` | `max_open_file_descriptors` | `-1` |
| `maxSharedDataValueSize` | `max_shared_data_value_size` | `10240` |
| `maxMgrRegisterStagger` | `max_mgr_register_stagger` | `0.0` |
| `numStartupRetries` | `num_startup_retries` | `60` |

Two more fields are diagnostic, not configuration: `print_lines` captures the
`CONFIG_INFO_MSG` output of `ServerAppOptionIniter::printAll` (one line per
configured option) and `unread_options` lists every path that had to fall back
to its default.

`serverMode` accepts `standalone` (default), `center` and `periphery`. See
[process topology](/architecture/process-topology).

## Resource paths

Resource locations are not in `bw.xml`; they come from `BWResource` in
`crates/resources/src/bwresource.rs`, in this precedence:

1. `--res` / `-r <path>` command line arguments
2. the deprecated `BW_RES_PATH` environment variable
3. the first usable line of `~/.bwmachined.conf` for `getUserId()`'s user

Paths are tokenised on `:` (`BW_RES_PATH_SEPARATOR` on unix). A trailing `+key`
in the resource path flag position is dropped because the loop runs to `argc - 1`.
The resource cache size is overridden by `BW_CACHE_SIZE`.

## Reading a value

Typed lookups all funnel through `get_section` (first chain entry that opens the
path wins) and then the default-taking `as` conversion:

```rust
let hertz: i32 = config.get_int("Server/gameUpdateHertz", 10);
let production: bool = config.get_bool("Server/production", true);
let personality: String = config.get_string("Personality/name", "BWPersonality");
```

Every lookup returns the default when no file in the chain defines the path.

## Sample files

::: warning No sample configuration ships in the repository
There is currently no `config/` directory and no `.xml` sample anywhere in the
tree (planned target directory is `config/`, `todo.md`). A runnable sample is
served by this site as a static resource instead:
[bw.standalone.xml](/examples/bw.standalone.xml).
:::

The sample is not illustrative filler: every value in it was checked against the
defaults in `crates/server-runtime/src/server_app.rs`, and the file parses as
well-formed XML. `production` is set to `false` on purpose — that is the one
value that departs from the source default, because `false` enables the
development diagnostics while you are still wiring things up.

Copy it to `<res>/server/bw.xml` and pass the directory as a resource path:

```bash
mkdir -p server
cp /path/to/docs/public/examples/bw.standalone.xml server/bw.xml
./target/debug/boundless-server --res "$(pwd)" loginapp
```

Until BW-14 lands a real `config/` directory with the FantasyDemo resources,
this page's option table and the sample above are the whole specification.