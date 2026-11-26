# OPT-15-01-03 — 检验平台针对性优化 (validate the platform-targeted optimizations)

验收: **相同功能和协议套件通过** (the same functionality and protocol suite
passes).

The tree's platform-targeted optimization is the Linux `sendmmsg`/`recvmmsg`
batch pair behind `platform::socket` (the OPT-02-01-03 delivery).
`crates/platform/tests/platform_opt_parity.rs` validates it the way the
acceptance asks: the identical datagram scenarios run through the generic
one-datagram-per-syscall arm (`sendto`/`recvfrom`) and the batched arm must
produce identical observable outcomes. It is not a second implementation of
the protocol — it is the same protocol suite, executed on both paths.

## The suite (256 datagrams, one loopback UDP pair)

Payload `seq` carries a big-endian sequence tag plus a deterministic fill at
a per-sequence length (16..=1200 bytes) — the varying lengths are what catch
coalescing and splitting. Both arms send the same 256 datagrams in paced
windows of 32 and drain each window before the next (the in-flight cap the
kernel's receive buffer dictates).

## What passed (structural, asserted)

- **Parity, datagram for datagram**: the batched arm hands over exactly what
  the generic arm hands over — same count (256), same order, byte-identical
  payloads, identical source port and address (network byte order). Both
  arms ride the *same* sender socket, so the source triples are comparable.
- **The sequence tag inside each payload is its own index** — in-order
  delivery under the paced window.
- **The optimization's syscall contract**: generic arm 256 send + 256 recv
  syscalls; batched arm **8 send + 8 recv syscalls** — one syscall per
  window, strictly fewer receives than datagrams.
- **Edge inputs answer as the generic API**: an empty batch sends nothing;
  an empty receive request reads nothing; a quiet nonblocking socket yields
  the *same* drained error from both arms (`recvfrom` and `recvmmsg` both
  return -1 with `errno == EAGAIN` — the wrappers surface -1 with errno set,
  which is the convention the mercury callers already handle via
  `last_os_error`); an 8-slot request over 5 queued datagrams returns
  exactly 5, byte-identical in slot order; a single-datagram batch rides the
  same contract as `sendto` (recvfrom hands back the batched datagram with
  the sender's ephemeral port and the loopback source address).

## Two findings the suite itself surfaced

1. **Drained-error shape**: the wrappers return the libc convention (-1 with
   errno), not the raw-syscall `-errno` the `sendmmsg` doc comment claims.
   The existing callers already read errno via `last_os_error()`, so nothing
   regresses — but the parity suite pins the *actual* shape both arms share,
   and the doc comment's "-errno" wording is now known to be aspirational
   (left as-is; fixing it is a comment-only change on another row's face).
2. **Slot width is the batch's capacity**: a 4-slot `recvmmsg` over 5 queued
   datagrams returns 4 — the unrequested datagram is not lost, it stays
   queued (verified with a standalone C repro against the kernel, which
   returns 5 when given 5 slots). The suite pins the width contract: request
   width bounds the batch, drain loops finish the window.

## Gates

- `cargo test -p platform` — 64 lib + `io_timer_baseline` +
  `platform_opt_parity` (2 tests) + `signal_call_sites`, all green.
- `cargo fmt -p platform -- --check`: clean.
- `cargo clippy -p platform --all-targets -- -D warnings`: no crate lint.