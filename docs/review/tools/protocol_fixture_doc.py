#!/usr/bin/env python3
"""Emit the BW-00-04-01 evidence markdown from the protocol-fixtures JSON.

Usage: protocol_fixture_doc.py PROTOCOL_FIXTURES_JSON > bw00-04-01-protocol-fixtures.md
The per-message table is regenerated from the JSON so the doc never disagrees
with the captured pcap.
"""
import json
import sys

# Message id -> reference description (machine_guard.hpp).
MEANING = {
    1: "tool/server -> machined",
    2: "process discovery (register / find / birth / death)",
    3: "per-process stats poll",
    4: "watch-nub listener query",
    7: "tag set / query",
    12: "interface version query",
    64: "machined to machined announce",
}


def main():
    with open(sys.argv[1], encoding="utf-8") as fh:
        d = json.load(fh)

    print("# BW-00-04-01 协议二进制夹具（machined）")
    print()
    print("> 证据面（BW-00-04-01 验收：记录消息 ID、方向及原始字节）。")
    print()
    print("由 `tools/protocol_fixture_scan.py` 在参考仓自带的 "
          "`scripts/cluster-smoke/deep-chain.sh` 全栈拉起期间抓取 UDP 20018 "
          "（`lib/network/portmap.hpp` 的 `PORT_MACHINED`）流量；"
          "源数据 `reference/bw00_04_01_protocol_fixtures.json`，"
          "原始报文另存 `reference/bw00_04_01_machined.pcap.gz`。")
    print()
    print("| 项 | 值 |")
    print("| --- | --- |")
    print(f"| 参考仓提交 | `{d['source_commit']}` |")
    print(f"| 捕获时间 | `{d['captured_at']}` |")
    print(f"| 捕获模式 | `{d['mode']}`"
          f"（`owned`=本扫描拉起并拆除集群；`live`=集群已有、只旁听） |")
    print(f"| 拉起 | rc={d['bringup_rc']}（{d['bringup_seconds']}s），"
          f"拆除 rc={d['teardown_rc']} |")
    print(f"| 捕获窗口 | {d['capture_seconds']}s，"
          f"20018 端口 {d['datagrams_on_20018']} 个报文，"
          f"{len(d['fixtures'])} 个（消息 ID, 方向）键 |")
    print()

    print("## 消息清单")
    print()
    print("| 消息 | ID | 方向 | 次数 | 含义 | 原始字节样本 |")
    print("| --- | --- | --- | --- | --- | --- |")
    for f in d["fixtures"]:
        sample = f["samples"][0] if f["samples"] else {}
        hexb = sample.get("hex", "—")
        if len(hexb) > 40:
            hexb = hexb[:40] + "…"
        meaning = MEANING.get(f["message_id"], "—")
        print(f"| `{f['message']}` | {f['message_id']} | {f['direction']} | "
              f"{d['census'].get('%d/%s' % (f['message_id'], f['direction']), 0)}"
              f" | {meaning} | `{hexb}` |")
    print()

    print("## 帧格式（按观测到的线路字节）")
    print()
    print("每个 UDP 报文是一个 `MGMPacket`（`lib/network/machine_guard.cpp` "
          "`MGMPacket::read`）：")
    print()
    print("- 报文头：`u8 packet flags` + `u32 buddy`（小端）。")
    print("- 之后按 `[u16 消息长度][消息体]` 重复；消息体以 "
          "`u8 message` + `u8 flags` + `u16 seq` 开头，随后是子类字段"
          "（`process`/`listener`/`signal` 等，`lib/network/machine_guard.hpp`）。")
    print("- 方向以 20018 端口为参照：目的端口 20018 为 `in`，源端口 20018 为 `out`"
          "（`MACHINED_ANNOUNCE` 是 machined→machined 广播，也落在目的 20018，"
          "故记为 `in`）。")
    print()
    print("**线路与源不符的 delta（已登记）**：参考**二进制**把每条消息的长度字段"
          "写为**小端**（观测一致），而参考**源码** `machine_guard.cpp:114` 用 "
          "`BW_HTONS`（大端）。夹具按观测线路记录；移植侧（`crates/contracts` 的 "
          "`machined.rs`，`Header`/`ProcessMessage`）应以二进制线路为准核对，"
          "不能照抄源码的字节序。")
    print()
    print("## 可追溯性")
    print()
    print(f"- 消息 ID 表：`lib/network/machine_guard.hpp` "
          f"`MachineGuardMessage::Message`（与 `crates/contracts/src/machined.rs` "
          f"`message_id` 逐项一致：1..15 与 64）。")
    print(f"- 完整报文：`reference/{d.get('pcap_archive') or '—'}`。")
    print()
    print("生成：`protocol_fixture_scan.py` + `protocol_fixture_doc.py`。")
    return 0


if __name__ == "__main__":
    sys.exit(main())