#!/usr/bin/env python3
"""Emit the BW-00-02-04 evidence markdown from the client-login JSON.

Usage: client_login_doc.py CLIENT_LOGIN_JSON > bw00-02-04-client-login.md
The request/callback tables are regenerated from the JSON so the doc never
disagrees with the captured run.
"""
import json
import sys


def main():
    with open(sys.argv[1], encoding="utf-8") as fh:
        d = json.load(fh)

    ev = d["client_events"]
    cb = d["server_callbacks"]
    print("# BW-00-02-04 最小客户端登录")
    print()
    print("> 证据面（BW-00-02-04 验收：留存请求和回调序列）。")
    print()
    print("由 `tools/client_login_scan.py` 驱动参考仓示例客户端"
          "（`examples/client_integration/python/simple` → `simple_python_client`）"
          "登录运行中的单机集群，抓取客户端请求/回调与服务端脚本回调；"
          "源数据 `reference/bw00_02_04_client_login.json`。")
    print()
    print("| 项 | 值 |")
    print("| --- | --- |")
    print(f"| 参考仓提交 | `{d['source_commit']}` |")
    print(f"| 捕获时间 | `{d['captured_at']}` |")
    print(f"| 客户端命令 | `{d['command']}` |")
    print(f"| BW_RES_PATH | `{d['res_path']}` |")
    print(f"| 退出码 / 耗时 | `{d['returncode']}` / {d['seconds']}s |")
    print(f"| 客户端源地址 | `{d['client_source_port']}` |")
    print(f"| 本机账号 | `MyAccount` / `a`（示例 `db.xml` 的 LogOnMapping，"
          f"实体 `ClientAvatar`=`MyCharacter`，实体 id {cb['proxy_id']}） |")
    print()

    print("## 请求序列")
    print()
    print("客户端按 `loginapp:20013` UDP → `baseapp` UDP 两跳完成登录"
          "（`-autoDisconnect` 使连上后即退出，故运行有界）：")
    print()
    print("| # | 类型 | 客户端日志 |")
    print("| --- | --- | --- |")
    for i, e in enumerate(ev, 1):
        print(f"| {i} | {e['kind']} | `{e['msg']}` |")
    print()

    print("## 服务端日志匹配（按客户端源端口）")
    print()
    print("| 服务端 | 日志行 |")
    print("| --- | --- |")
    for s in d["server_events"]:
        print(f"| `{s['server']}` | `{s['line']}` |")
    print()

    print("## 服务端回调序列（本运行的实体 id %s）" % cb["proxy_id"])
    print()
    print("`baseapp.log` / `baseapp2.log`（登录可能落在第二个 BaseApp 实例，"
          "两个日志一并切分）：")
    print()
    for l in cb["baseapp"]:
        print(f"- `{l}`")
    print()
    print("`cellapp.log`：")
    print()
    for l in cb["cellapp"]:
        print(f"- `{l}`")
    print()

    print("## 说明")
    print()
    print("- **两跳协议**：LoginApp 回复后转 BaseApp；`onLoginAppLoginSuccess` 与 "
          "`onBaseAppLoginSuccess` 即两次握手的回调，`LOGGED_ON` → "
          "`enableEntities` 后实体（ClientAvatar）随 proxy 下发。")
    print("- **NAT 观察**：客户端经 `127.0.0.1:20013` 连 LoginApp，baseapp 记 "
          "`Addresses are different (due to NAT?)`——同机双路径（回环 + 网卡地址）。")
    print("- **`REASON_NO_SUCH_PORT`**：`-autoDisconnect` 断开后客户端端口已释放，"
          "服务端再发包时的正常告警，随 `Proxy::onClientDeath`/`removeProxy` 收尾。")
    print("- **日志归属**：`baseapp*.log`/`cellapp.log` 为追加式，本运行的回调行按"
          "实体 id（`Base::Base(%s): ClientAvatar`）切分。" % cb["proxy_id"])
    print()
    print("生成：`client_login_scan.py` + `client_login_doc.py`。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
