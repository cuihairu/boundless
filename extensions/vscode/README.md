# Boundless for VS Code

Entity definition support, local cluster tasks and documentation access for
Boundless workspaces. The language side covers `.def` files (`entities.xml`,
`alias.xml` and friends): diagnostics, snippets and a jump from an entity
element to the role script that really carries it. The cluster side starts and
stops roles locally and shows their logs. The documentation side hands the
repository's documentation to an editor with an MCP client.

## Install

From a built package:

```sh
pnpm run package                      # writes boundless-vscode-0.0.1.vsix
code --install-extension boundless-vscode-0.0.1.vsix
```

`code --install-extension` accepts the file in any editor that takes VSIX files;
the package carries only what the editor needs, so it installs into a profile
that has nothing else.

Building it needs the toolchain of the folder:

```sh
cd extensions/vscode
pnpm install
pnpm run build                       # out/
pnpm run check                       # typecheck of src/ and test/
pnpm test                            # unit tests
pnpm run verify                      # end to end, over real processes
```

## Enable it in an empty workspace

The extension does not look for a project in a fixed place: it walks the
workspace folder for the resource root, which is the directory that holds
`scripts/entities.xml`. Open any folder that contains one, or point the
workspace at it, and

> **Boundless: Show Workspace Configuration**

reports what it found, one line per workspace folder:

```
folder: /work/game
  res root:   /work/game/res
  entities:   /work/game/res/scripts/entities.xml
  alias:      /work/game/res/scripts/entity_defs/alias.xml
  defs:       /work/game/res/scripts/entity_defs
  scripts:    base/cell/client/service
```

An empty folder reports `No Boundless resource root found.` — that is the
extension telling you there is nothing to bind to yet, not a failure.

## Commands

| Command | What it does |
| --- | --- |
| **Boundless: Show Workspace Configuration** | Locates the resource root of every workspace folder. |
| **Boundless: Start Local Cluster** | Starts the configured roles, each in its own process group. |
| **Boundless: Stop Local Cluster** | Stops them, children included. |
| **Boundless: Show Cluster Status** | Lists the running roles, their pids and log paths. |
| **Boundless: Show Role Log** | Opens the log of the role you pick. |
| **Boundless: Run Tests** | Runs `boundless.cluster.testsCommand` in the workspace folder. |
| **Boundless: Run Bots** | Runs `boundless.cluster.botsCommand`; empty disables the job. |
| **Boundless: Set Up Python Debug Configurations** | Adds a `debugpy` attach configuration per role to `launch.json`. |
| **Boundless: Set Up Rust Launch Configurations** | Adds a launch configuration per role, with this platform's debugger. |
| **Boundless: Set Up Documentation MCP Server** | Writes `.vscode/mcp.json` for the documentation server. |

`launch.json` and `mcp.json` are merged, never replaced: entries you wrote
yourself stay, and the entries this extension wrote are replaced rather than
duplicated when you run the command again.

## Settings

Cluster:

| Setting | Default | Meaning |
| --- | --- | --- |
| `boundless.cluster.binary` | `target/debug/boundless-server` | The role binary, relative to the folder holding the resource root. |
| `boundless.cluster.roles` | `["baseapp", "cellapp"]` | Roles the cluster starts. |
| `boundless.cluster.testsCommand` | `cargo test` | What **Run Tests** executes. |
| `boundless.cluster.botsCommand` | `""` | What **Run Bots** executes; empty disables it. |
| `boundless.cluster.launchRoles` | `["baseapp", "cellapp"]` | Roles that get a debug configuration. |

Documentation:

| Setting | Default | Meaning |
| --- | --- | --- |
| `boundless.docs.server` | `integrations/mcp/docs/target/release/boundless-docs-mcp` | The documentation server to start, or an absolute path. |
| `boundless.docs.root` | `${workspaceFolder}` | The git repository the documentation is read from. |
| `boundless.docs.version` | `HEAD` | The ref labels the queries answer for. |
| `boundless.docs.prefix` | `docs` | The documentation directory inside the repository. |
| `boundless.docs.langs` | `["zh"]` | Locale directories inside it. |
| `boundless.docs.url` | `""` | The base URL of the rendered site; empty keeps source links repository-relative. |

Queries answer for the ref the workspace is at, which is why `HEAD` is the
default: a question about the documentation always lands on the documentation
the checked-out code carries. Point `boundless.docs.version` at an older tag to
ask for that edition instead.

## The documentation server

Build it once, where the extension expects it:

```sh
cd integrations/mcp/docs
cargo build --release
```

Then **Boundless: Set Up Documentation MCP Server** writes, per workspace folder:

```json
{
  "servers": {
    "boundless-docs": {
      "type": "stdio",
      "command": "${workspaceFolder}/integrations/mcp/docs/target/release/boundless-docs-mcp",
      "args": ["--root", "${workspaceFolder}", "--prefix", "docs",
               "--version", "HEAD", "--langs", "zh"]
    }
  }
}
```

An editor that reads `.vscode/mcp.json` picks it up without further setup; the
server then answers `search_docs` and `read_doc`, each hit carrying the page,
the section and the ref it was read from, and `read_doc` also returning the
rendered source link of the section.

## Verification

`pnpm run verify` builds the compiled extension and drives it: a role binary is
started and stopped for real (leaving no process behind), a failing job reports
its exit status and results path, the documentation server is started from the
configuration the extension wrote and answered for the ref the workspace is at,
the VSIX is packaged and its contents checked, and the packaged extension is
run inside a host over a demo workspace.
