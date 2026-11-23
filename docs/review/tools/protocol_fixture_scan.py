#!/usr/bin/env python3
"""Capture machined-protocol binary fixtures from the reference stack (BW-00-04-01).

The acceptance asks for fixtures that record, per protocol message, its id,
direction and raw bytes. The machined process-discovery protocol is the one
protocol both stacks speak on the wire today (u8 message, u8 flags, u16 seq
inside an MGMPacket datagram on UDP 20018; reference
lib/network/machine_guard.hpp/.cpp), so this scan:

  1. arms tcpdump on UDP 20018 (all interfaces) under `sudo -n timeout`,
     bounded so the capture can never run away;
  2. brings the reference cluster up with its own deep-chain.sh (every app
     registers with bwmachined2 on the way) and tears it down again (the
     teardown chatter — deregisters and death notices — is part of the
     fixture surface);
  3. parses the pcap in pure python (pcap records, LINUX_SLL/EN10MB link
     layer, IPv4 + UDP) and decodes every MGMPacket: packet flags, buddy
     address, then [u16 BE length][message: u8 id, u8 flags, u16 LE seq,
     body];
  4. records one fixture entry per (message id, direction) with up to four
     distinct raw bodies, plus a full per-message census.

Direction is relative to bwmachined2: packets addressed to UDP 20018 are
"in" (tool/server -> machined), packets sent from 20018 are "out".

Usage: protocol_fixture_scan.py BIGWORLD_ROOT OUTPUT_JSON [PCAP_GZ]
"""
import gzip
import json
import os
import struct
import subprocess
import sys
import tempfile
import time

# lib/network/machine_guard.hpp MachineGuardMessage::Message
MESSAGE_NAMES = {
    1: "WHOLE_MACHINE", 2: "PROCESS", 3: "PROCESS_STATS", 4: "LISTENER",
    5: "CREATE", 6: "SIGNAL", 7: "TAGS", 8: "USER", 9: "PID", 10: "RESET",
    11: "ERROR", 12: "QUERY_INTERFACE", 13: "CREATE_WITH_ARGS",
    14: "HIGH_PRECISION_MACHINE", 15: "MACHINE_PLATFORM",
    64: "MACHINED_ANNOUNCE",
}
MACHINED_PORT = 20018
SAMPLES_PER_KEY = 4


def run(cmd, timeout=120, **kw):
    return subprocess.run(cmd, capture_output=True, text=True,
                          timeout=timeout, **kw)


def pcap_link_payloads(path):
    """Yield (ts, ip_packet_bytes) for every record in a pcap file."""
    data = open(path, "rb").read()
    if len(data) < 24:
        return
    magic = data[:4]
    if magic == b"\xa1\xb2\xc3\xd4":
        endian = ">"
    elif magic == b"\xd4\xc3\xb2\xa1":
        endian = "<"
    else:
        raise ValueError("not a classic pcap file (magic %s)" % magic.hex())
    linktype = struct.unpack_from(endian + "I", data, 20)[0]
    off = 24
    while off + 16 <= len(data):
        ts_sec, ts_usec, incl_len, _ = struct.unpack_from(
            endian + "IIII", data, off)
        off += 16
        pkt = data[off:off + incl_len]
        off += incl_len
        ts = ts_sec + ts_usec / 1e6
        for payload in link_ip_payloads(pkt, linktype):
            yield ts, payload


def link_ip_payloads(pkt, linktype):
    """Strip the link layer; yield the IPv4 packet when the ethertype is IP."""
    if linktype == 276:  # LINUX_SLL2 (-i any on this host, 20-byte header)
        if len(pkt) < 20:
            return
        if struct.unpack_from(">H", pkt, 0)[0] == 0x0800:
            yield pkt[20:]
    elif linktype == 113:  # LINUX_SLL (16-byte header)
        if len(pkt) < 16:
            return
        if struct.unpack_from(">H", pkt, 14)[0] == 0x0800:
            yield pkt[16:]
    elif linktype == 1:  # EN10MB
        if len(pkt) < 14:
            return
        if struct.unpack_from(">H", pkt, 12)[0] == 0x0800:
            yield pkt[14:]
    elif linktype == 101:  # RAW IP
        yield pkt


def udp_of(ip):
    """(src_port, dst_port, payload) of an IPv4/UDP packet, or None."""
    if len(ip) < 28 or (ip[0] >> 4) != 4 or ip[9] != 17:
        return None
    ihl = (ip[0] & 0xF) * 4
    udp = ip[ihl:]
    if len(udp) < 8:
        return None
    sport, dport, ulen = struct.unpack_from(">HHH", udp, 0)
    return sport, dport, udp[8:8 + max(0, ulen - 8)]


def parse_mgm_packet(payload):
    """Decode one MGMPacket datagram (machine_guard.cpp MGMPacket::read)."""
    if len(payload) < 5:
        return None
    pkt_flags = payload[0]
    buddy = struct.unpack_from("<I", payload, 1)[0]
    msgs, off = [], 5
    while off + 2 <= len(payload):
        # OBSERVED framing: the reference *binary* writes msglen little-endian
        # here, while the tree source machine_guard.cpp:114 applies BW_HTONS
        # (big-endian). The fixture records the observed wire; the source /
        # binary delta is logged in the evidence doc.
        msglen = struct.unpack_from("<H", payload, off)[0]
        off += 2
        if off + msglen > len(payload):
            break
        body = payload[off:off + msglen]
        off += msglen
        if msglen >= 4:
            msgs.append({
                "message": body[0], "flags": body[1],
                "seq": struct.unpack_from("<H", body, 2)[0],
                "len": msglen, "hex": body.hex(),
            })
    return pkt_flags, buddy, msgs


def main():
    if len(sys.argv) < 3:
        print(__doc__)
        return 1
    root, out_json = sys.argv[1], sys.argv[2]
    pcap_gz = sys.argv[3] if len(sys.argv) > 3 else None
    smoke = os.path.join(root, "scripts", "cluster-smoke", "deep-chain.sh")

    with tempfile.TemporaryDirectory(prefix="bwproto.") as tmp:
        pcap = os.path.join(tmp, "machined.pcap")

        # Live mode: a cluster someone else owns is left alone. Tearing down a
        # foreign cluster collides with whoever drives it, so when bwmachined2
        # is already listening the scan captures its steady-state chatter
        # instead of bringing a cluster up and down again.
        live = bool(subprocess.run(
            ["pgrep", "-x", "bwmachined2"], capture_output=True,
            text=True).stdout.strip())
        mode = "live" if live else "owned"
        cap_secs = 60 if live else 90
        # Bounded capture: `timeout` owns tcpdump, so the capture can never
        # outlive this script, and SIGINT makes tcpdump flush the pcap.
        cap = subprocess.Popen(
            ["sudo", "-n", "timeout", "--signal=INT", str(cap_secs),
             "tcpdump", "-i", "any", "-nn", "-w", pcap,
             "udp port %d" % MACHINED_PORT],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        time.sleep(1)

        bringup_rc = teardown_rc = None
        t0 = time.time()
        if mode == "owned":
            up = run(["bash", smoke], timeout=150)
            bringup_rc = up.returncode
            run(["sleep", "6"])
            down = run(["bash", smoke, "--teardown"], timeout=90)
            teardown_rc = down.returncode
        up_secs = round(time.time() - t0, 1)
        cap.wait(timeout=cap_secs + 15)

        fixtures, census, datagrams = {}, {}, 0
        for ts, ip in pcap_link_payloads(pcap):
            udp = udp_of(ip)
            if udp is None:
                continue
            sport, dport, payload = udp
            if dport == MACHINED_PORT:
                direction = "in"
            elif sport == MACHINED_PORT:
                direction = "out"
            else:
                continue
            decoded = parse_mgm_packet(payload)
            if decoded is None:
                continue
            datagrams += 1
            for m in decoded[2]:
                key = (m["message"], direction)
                census[key] = census.get(key, 0) + 1
                entry = fixtures.setdefault(key, {
                    "message_id": m["message"],
                    "message": MESSAGE_NAMES.get(m["message"], "UNKNOWN"),
                    "direction": direction, "samples": [],
                })
                seen = {s["hex"] for s in entry["samples"]}
                if len(entry["samples"]) < SAMPLES_PER_KEY \
                        and m["hex"] not in seen:
                    entry["samples"].append({
                        "flags": m["flags"], "seq": m["seq"],
                        "len": m["len"], "hex": m["hex"],
                    })

        if pcap_gz:
            with open(pcap, "rb") as src, gzip.open(pcap_gz, "wb") as dst:
                dst.writelines(src)

    data = {
        "source_commit": run(["git", "-C", root, "rev-parse",
                              "--short", "HEAD"]).stdout.strip(),
        "captured_at": time.strftime("%Y-%m-%d %H:%M:%S %z"),
        "smoke_script": smoke,
        "mode": mode,
        "bringup_rc": bringup_rc, "bringup_seconds": up_secs,
        "teardown_rc": teardown_rc,
        "capture_seconds": cap_secs,
        "datagrams_on_20018": datagrams,
        "census": {"%d/%s" % k: v for k, v in sorted(census.items())},
        "fixtures": [fixtures[k] for k in sorted(fixtures)],
        "pcap_archive": os.path.basename(pcap_gz) if pcap_gz else None,
    }
    with open(out_json, "w", encoding="utf-8") as fh:
        json.dump(data, fh, indent=1, sort_keys=True)

    print("commit %s  mode=%s  capture=%ss  bringup rc=%s (%ss)  "
          "teardown rc=%s" % (
              data["source_commit"], mode, cap_secs, bringup_rc, up_secs,
              teardown_rc))
    print("datagrams on 20018: %d  distinct (id,direction) keys: %d" % (
        datagrams, len(fixtures)))
    for key, count in sorted(census.items()):
        print("  msg %-2d %-3s x%-4d %s" % (key[0], key[1], count,
                                            MESSAGE_NAMES.get(key[0], "?")))
    return 0


if __name__ == "__main__":
    sys.exit(main())