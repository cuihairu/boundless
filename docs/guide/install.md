# Installing on three platforms

Boundless targets three platforms for its first batch of build targets
(BW-15): **Linux x86-64**, **Windows x86-64** and **macOS arm64**. This page
covers building the server from source on each.

## Status per platform

| Platform | Build path documented here | Verified | Tracked by |
| --- | --- | --- | --- |
| Linux x86-64 | Yes, fully | Yes, this tree builds | BW-15-02 |
| Windows x86-64 | Prerequisites only | Not verified here | BW-15-03 |
| macOS arm64 | Prerequisites only | Not verified here | BW-15-04 |

Windows and macOS items are listed as *not started* in `todo.md`, and
`crates/platform/` currently contains only `clock.rs`. The build instructions
below for those two platforms describe the intended path and have not been run
against this tree.

## Prerequisites

| Need | Linux x86-64 | Windows x86-64 | macOS arm64 |
| --- | --- | --- | --- |
| Rust toolchain | **1.100.0** (pinned) | **1.100.0** | **1.100.0** |
| Package manager | cargo | cargo | cargo |
| C/C++ compiler | for any native deps | MSVC or clang-cl | clang (Xcode CLT) |
| Python | CPython 3.13.16 for embedded scripts | same | same |

The toolchain is pinned in `rust-toolchain.toml`:

```toml
[toolchain]
channel = "1.100.0"
```

This exists so a local checkout and CI resolve the same compiler version, so do
not bypass it with `rustup override`.

## Linux x86-64

```bash
# 1. Install rustup and the pinned toolchain
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup toolchain install 1.100.0

# 2. Clone and build the server target
git clone <boundless-url> && cd boundless
cargo build -p boundless-server

# 3. Location of the resulting binary
ls target/debug/boundless-server
```

A release build uses the workspace profile `opt-level = 3` with `debug = true`:

```bash
cargo build --release -p boundless-server
```

## Windows x86-64 and macOS arm64

The same two commands apply once the toolchain is installed. Nothing
platform-specific is encapsulated yet, which is exactly why BW-15-01 exists:
event polling, monotonic clock, signal handling, subprocess control, file
locking, memory mapping and path/encoding checks must all be behind
`crates/platform/` before a third-party build can be trusted. Only the
monotonic clock is there today.

## Clean-install checklist

BW-14-04-04 requires that a clean environment can install, start and stop.
Reproduce it like this:

1. Fresh container or fresh user account, no prior `~/.cargo`.
2. Install the pinned toolchain (step 1 above).
3. `cargo build -p boundless-server` must finish without error.
4. `target/debug/boundless-server loginapp` must print a startup line and
   exit `0`; `target/debug/boundless-server bogus` must exit `1`.

Steps 3 and 4 are what this page actually verifies today. Resource loading,
login and persistence need the items they are tracked under before they can be
part of the checklist.