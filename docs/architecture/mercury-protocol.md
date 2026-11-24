# Mercury protocol

Mercury is the reference's reliable-UDP messaging layer (`lib/network/`);
`crates/mercury` is its port. This page records the wire format, the
channel/retransmission machinery, connection establishment, the message layer
and encryption — and, just as importantly, which parts of the crate are wired
into the build today.

## What is wired

`crates/mercury/src/lib.rs` declares 37 modules; 56 non-lib source files sit
in `crates/mercury/src/`. The following **19 files are not declared** and are
therefore not compiled by any target:

`encryption_stream_filter.rs`, `exposed_message_range.rs`,
`initial_connection_filter.rs`, `irregular_channels.rs`, `message_frame.rs`,
`packet_receiver_stats.rs`, `packet_sender.rs`, `rate_limiter.rs`,
`recently_dead_channels.rs`, `reliable_order.rs`,
`symmetric_block_cipher.rs`, `tcp_bundle_dispatch.rs`,
`tcp_channel_lifecycle.rs`, `tcp_connection_header.rs`, `tcp_frame.rs`,
`tcp_send_buffer.rs`, `tcp_send.rs`, `tcp_transport_tests.rs`,
`udp_bundle_processor.rs`.

Notable consequences: the Blowfish cipher
(`symmetric_block_cipher.rs`) and the TCP stream encryption filter
(`encryption_stream_filter.rs`) are present but unwired (the `blowfish`
workspace dependency is not listed in `crates/mercury/Cargo.toml`); the
generic `RateLimiter` and the 60-second recently-dead-channel guard are
unwired (the live receive-side limiter is `rate_limit_message_filter.rs`).
The standalone `initial_connection_filter.rs` has a live twin inside
`crates/mercury/src/packet_filter.rs`, which is the one
`network_interface.rs` constructs.

The core transport is Linux-only — `network_interface.rs`, `udp_channel.rs`,
`poller.rs`, `packet_receiver.rs`, `channel_map.rs`, `event_dispatcher.rs`,
`condemned_channels.rs` and `channel_listener.rs` are behind
`#[cfg(target_os = "linux")]` because the reference's `EPoller` backend
(`HAS_EPOLL`) is the only poll backend ported (`crates/mercury/src/lib.rs`
doc comment on `poller`).

`todo.md` records the BW-02 lane as 59 of 60 rows checked; the single
unchecked row, BW-02-05-04 (configured rate limiting), also appears a second
time in the same subsection *checked* with evidence pointing at
`rate_limit_message_filter.rs` — the two rows disagree, and the module they
point at is wired and tested.

## Packet format

The packet is a fixed 1472-byte buffer in `crates/mercury/src/packet.rs`:

| Constant | Value | Meaning |
| --- | --- | --- |
| `PACKET_MAX_SIZE` | 1472 | UDP payload budget (below the MTU) |
| `HEADER_SIZE` | 2 | flags, 2 little-endian bytes at offset 0 |
| `RESERVED_FOOTER_SIZE` | 27 | space reserved before writing (acks 1+4·n, requests 2, fragments 8, ...) |
| `MAX_CAPACITY` | 1443 | message payload per packet, pinned by test |
| `MAX_ACKS` | 255 | per-packet ack footer limit |

The header grows forward from offset 0; **footers grow backwards** from
`msg_end_offset`. Flag bits (`Flags` module, `crates/mercury/src/packet.rs`):

| Flag | Value |
| --- | --- |
| `HAS_REQUESTS` | 0x0001 |
| `HAS_PIGGYBACKS` | 0x0002 |
| `HAS_ACKS` | 0x0004 |
| `ON_CHANNEL` | 0x0008 |
| `IS_RELIABLE` | 0x0010 |
| `IS_FRAGMENT` | 0x0020 |
| `HAS_SEQUENCE_NUMBER` | 0x0040 |
| `INDEXED_CHANNEL` | 0x0080 |
| `HAS_CHECKSUM` | 0x0100 |
| `CREATE_CHANNEL` | 0x0200 |
| `HAS_CUMULATIVE_ACK` | 0x0400 |

`KNOWN_FLAGS = 0x07FF`; unknown bits mark the packet corrupt.
`UdpBundle::prepare_packets` in `crates/mercury/src/udp_bundle.rs` writes the
footers innermost-first: checksum (4 bytes, only when the interface asks),
request offset (2), fragment pair (`fragEnd` then `fragBegin`, u32 each),
acks (1 + 4·n), channel flags (indexed id/version, cumulative ack, ack list),
then the sequence number (u32) — set for reliable, fragment or external-channel
packets. Sequence numbers live on a 28-bit ring
(`SEQ_SIZE = 0x1000_0000`, half-ring comparison `seq_less_than`,
`SEQ_NULL = SEQ_SIZE`; `crates/mercury/src/misc.rs`).

Two formats worth calling out:

- **Piggyback chains** (`process_piggyback_packets`,
  `crates/mercury/src/packet.rs`): i16 lengths; the last block's length is
  one's-complemented (high bit set) to end the chain.
- **Checksum** (`write_checksum`/`validate_checksum`): XOR of every 32-bit
  little-endian word of the whole packet including the header.

### Byte order

All framing code and inline tests are little-endian
(`to_le_bytes`/`from_le_bytes` throughout `crates/mercury/src/message_framing.rs`),
and the module's test comment records why: "The byte order is the reference's
BW_HTONS/BW_HTONL, which are little-endian on this host" — quoting the
reference `binary_stream.ipp`: "BigWorld uses little-endian over the network,
not big-endian". The machine-readable baseline record
`tests/baseline/protocol-format.json` describes the packet length field as
big-endian, which does not match the compiled code; the code is the authority
until that record is corrected.

## Channels, retransmission, fragmentation

`crates/mercury/src/udp_channel.rs` is the reliable-UDP channel
(`UdpChannel`, one per remote `Address` in `crates/mercury/src/channel_map.rs`).
Window sizes follow the reference traits:

| Channel trait | Window |
| --- | --- |
| External | 256 (`EXTERNAL_CHANNEL_SIZE`) |
| Internal, unindexed | 4096 (`INTERNAL_CHANNEL_SIZE`) |
| Internal, indexed | 512 (`INDEXED_CHANNEL_SIZE`) |

Reliability state on the send side is a `CircularArray` of `UnackedPacket`
slots keyed by sequence number (`crates/mercury/src/unacked_packet.rs` —
packet, last-send out-seq, last-send time, `was_resent`, and the
`ReplyOrder` list that pins where reply ids live in the payload). The resend
pass (`check_resend_timers`, `udp_channel.rs`) re-sends a packet when a newer
ack has passed its slot, or — on irregular channels — when its age exceeds
2·RTT floored at the inactivity resend delay; a pass resends at most
`window_size / 8` packets (the code names 8 as the reference's magic number).
Ack handling (`handle_ack`) updates the RTT estimate as
`round_trip_time = (rtt*9 + delta) / 10`, applied only to packets that were
never resent, then frees the slot and slides `small_out_seq_at` forward.
Acks to send accumulate in a `BTreeSet` and go out in the footer (1 + 4·n,
capped at `MAX_ACKS`), plus a 4-byte cumulative ack when there is room.

The receive window is the mirror `CircularArray` of buffered packets
(`add_to_receive_window`, `udp_channel.rs`): in-order delivery walks the
packet chain, duplicates (seq below `in_seq_at`) are dropped, a packet from a
different source address is corrupt, and the window doubles under pressure
(`crates/mercury/src/circular_array.rs` — power-of-two slots, absolute-index
addressing).

Fragments: a bundle larger than `MAX_CAPACITY` is split into packets sharing
an `IS_FRAGMENT` footer; reassembly lives in
`crates/mercury/src/fragmented_bundle.rs` — fragments are inserted in
sequence order, duplicates are discarded, mismatched fragment footers mark the
bundle mangled, and the whole assembly ages out at `MAX_AGE = 10` seconds.
`strip_frag_info` (`crates/mercury/src/packet.rs`) requires at least two
fragments with `seq` within `[frag_begin, frag_end]`.

## Connection establishment

There is no handshake state machine; establishment is composed of three
mechanisms:

1. **`CREATE_CHANNEL`** is set on the first reliable internal-channel packet
   (`write_flags`, `crates/mercury/src/udp_channel.rs`); the receiving
   `NetworkInterface` creates an anonymous channel for it.
2. **`InitialConnectionFilter`** — the live port is in
   `crates/mercury/src/packet_filter.rs`, constructed in
   `crates/mercury/src/network_interface.rs`. It admits exactly the 16-byte
   login probe (`flags == HAS_REQUESTS`, total size 16, first body byte the
   login message id: flags 2 + message id 1 + session key 4 + attempts 1 +
   reply id 4 + two offsets 4) and rejects everything else with
   `REASON_GENERAL_NETWORK`. The byte table is documented in the twin module
   `crates/mercury/src/initial_connection_filter.rs` (unwired).
3. **Encryption is fitted after key agreement** via
   `UdpChannel::set_encryption`, which wraps a `BlockCipherPtr` into the
   channel's `p_filter` (`crates/mercury/src/udp_channel.rs`).

A real loopback encryption handshake and a wrong-key rejection that leaves no
residual session are both covered by tests in
`crates/mercury/src/network_interface.rs`.

## Message layer

Message registration follows the reference shape:

- `crates/mercury/src/interface_table.rs` pre-allocates 256 message-id slots
  per socket.
- `crates/mercury/src/interface_minder.rs` assigns ids in **declaration
  order** (`id = elements.len()`), supports replicated id ranges, and caps
  `MAX_MESSAGE_ID` at 254 — id 255 is reserved for replies
  (`REPLY_MESSAGE_IDENTIFIER = 0xFF` in
  `crates/mercury/src/interface_element.rs`).
- `crates/mercury/src/interface_element.rs` carries the three length styles
  (fixed / variable / callback) and the variable-length header width rules.

A request frame is `[id:1][length:lengthParam][replyId:4][nextRequest:2][body]`
(`crates/mercury/src/message_framing.rs`). A length field of all ones is the
oversize escape: the true length moves to the head of the body and the first
4 body bytes are displaced to the tail (`special_compress_length` /
`special_expand_length`). Dispatch (`crates/mercury/src/bundle_dispatch.rs`)
walks bodies with packet-absolute request links translated as it goes, and
mirrors the reference's per-error strings ("Not enough data on stream at {}
for header...").

Replies are managed by `crates/mercury/src/request_manager.rs`: reply ids run
1..1,000,000 wrapping (`crates/mercury/src/misc.rs`), a request arms a 5-second
timeout timer only when it is channel-less, the pending request is removed
*before* the handler runs (one callback max), and cancelling a channel fails
its pending requests with `REASON_CHANNEL_LOST`. The `Reason` code table
(`Success = 0` through `MessageTooLong = -13`, string table indexed by
`-reason`) lives in `crates/mercury/src/misc.rs`.

Exposed-method sub-id ranges — the `msgIDFromExposedID` arithmetic behind the
entity RPC surface described in
[Entity pipeline](/architecture/entity-pipeline) — exist in
`crates/mercury/src/exposed_message_range.rs`, which is one of the unwired
files; its tests pin that the wire arithmetic packs **256** per overflow slot
even though the surrounding comments say 255.

## Encryption

`crates/mercury/src/block_cipher.rs` defines the cipher seam
(`encrypt_block`/`decrypt_block`/`key`/`block_size`, XOR block chaining by
default, a `NullCipher`). The wired filter is
`crates/mercury/src/encryption_filter.rs`:

- `ENCRYPTION_MAGIC: u32 = 0xdead_beef`; `MTU_ALLOWANCE = 200` spare bytes.
- On send: the magic and a padding wastage byte are written into the source
  packet's spare area and the ciphertext goes into a fresh packet.
- Chaining: each block after the first is XORed with the previous
  **plaintext** before encryption; decryption reverses it.
- On receive: decrypt in place, magic must match, wastage must be within one
  block, then the footer is shrunk away.

A session is encrypted by `UdpChannel::set_encryption`. The Blowfish
implementation (`symmetric_block_cipher.rs`, key sizes 4–56 bytes, carrying
the Eric Young known-answer vector) and the TCP stream variant
(`encryption_stream_filter.rs`) are both unwired.

## Event loop and plumbing

`crates/mercury/src/event_dispatcher.rs` runs the per-tick order:
frequent tasks, timers (unless broken), stats (unconditional), then network
processing under a time budget (default 0.01 s, idle wait 0.1 s).
`crates/mercury/src/packet_receiver.rs` drains the socket under a
`max_socket_processing_time` budget; `crates/mercury/src/network_stats.rs`
caches `/proc/net/udp` queue depths at a 400 ms interval.

## Compatibility anchors

Every module cites its `lib/network/` origin in its doc header (for example
`crates/mercury/src/unacked_packet.rs` → `lib/network/unacked_packet.{hpp,cpp}`,
with per-symbol `cpp:line` anchors inline). The baseline records live outside
the crate: `tests/baseline/protocol-format.json` pins the wire constants
against the reference tree, and `tests/baseline/protocol-capture.json` records
that no raw frame capture has been retained yet (capture gate closed as
"absent").

## State

| Piece | State |
| --- | --- |
| Packet/bundle codec, flags, checksum, piggyback | Implemented (`packet.rs`, `udp_bundle.rs`) |
| UDP endpoint, poller, receive drain | Implemented, Linux-only (`endpoint.rs`, `poller.rs`, `packet_receiver.rs`) |
| Channel lifecycle, windows, retransmission, acks | Implemented (`udp_channel.rs`, `channel_map.rs`, `unacked_packet.rs`) |
| Fragmentation and reassembly | Implemented (`fragmented_bundle.rs`) |
| Connection filter, anonymous channel creation | Implemented (`packet_filter.rs`, `network_interface.rs`) |
| Message framing, dispatch, replies, timeouts | Implemented (`message_framing.rs`, `bundle_dispatch.rs`, `request_manager.rs`) |
| Receive-side rate limiting (message filter) | Implemented (`rate_limit_config.rs`, `rate_limit_message_filter.rs`) |
| Stream cipher filter (session encryption) | Implemented (`block_cipher.rs`, `encryption_filter.rs`) |
| Real-loopback handshake / round-trip tests | Implemented (`network_interface.rs`, `udp_bundle.rs`) |
| Blowfish cipher, TCP stream encryption | On disk, **not wired** (`symmetric_block_cipher.rs`, `encryption_stream_filter.rs`) |
| Generic per-IP rate limiter, recently-dead guard, irregular channels, sender stats | On disk, **not wired** (`rate_limiter.rs`, `recently_dead_channels.rs`, `irregular_channels.rs`, `packet_sender.rs`) |
| Exposed-method message ranges | On disk, **not wired** (`exposed_message_range.rs`) |
| TCP transport (frame, connection header, send buffer, dispatch) | On disk, **not wired** (`tcp_*.rs`) |
| Raw reference frame captures | **Not recorded** (`tests/baseline/protocol-capture.json`) |
| Configured rate-limit acceptance (BW-02-05-04) | Checked/unchecked rows disagree in `todo.md`; module itself is wired |
