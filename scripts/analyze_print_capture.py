#!/usr/bin/env python3
"""Summarize AirPrint/IPP traffic in pktmon etl2pcap PCAPNG captures.

Only packet metadata, HTTP headers, and IPP attribute names/value types are emitted.
Document bodies and arbitrary payload bytes are never printed.
"""
import argparse
import ipaddress
import re
import struct
import sys
from datetime import datetime, timezone

PHONE = "192.168.3.197"
PORTS = {631, 8631}
IPP_OPS = {
    0x0002: "Print-Job", 0x0004: "Validate-Job", 0x0005: "Create-Job",
    0x0006: "Send-Document", 0x0008: "Cancel-Job",
    0x0009: "Get-Job-Attributes", 0x000A: "Get-Jobs",
    0x000B: "Get-Printer-Attributes",
}
IPP_STATUS = {
    0x0000: "successful-ok", 0x0001: "successful-ok-ignored-or-substituted-attributes",
    0x0002: "successful-ok-conflicting-attributes", 0x0400: "client-error-bad-request",
    0x0401: "client-error-forbidden", 0x0402: "client-error-not-authenticated",
    0x0403: "client-error-not-authorized", 0x0404: "client-error-not-possible",
    0x0405: "client-error-timeout", 0x0406: "client-error-not-found",
    0x0407: "client-error-gone", 0x0408: "client-error-document-format-not-supported",
    0x040B: "client-error-attributes-or-values-not-supported",
    0x0500: "server-error-internal-error", 0x0501: "server-error-operation-not-supported",
    0x0502: "server-error-service-unavailable", 0x0503: "server-error-version-not-supported",
}
VALUE_TYPES = {0x21: "integer", 0x22: "boolean", 0x23: "enum",
               0x30: "octetString", 0x31: "dateTime", 0x32: "resolution",
               0x33: "rangeOfInteger", 0x34: "collection", 0x35: "textWithLanguage",
               0x36: "nameWithLanguage", 0x41: "textWithoutLanguage",
               0x42: "nameWithoutLanguage", 0x44: "keyword", 0x45: "uri",
               0x46: "uriScheme", 0x47: "charset", 0x48: "naturalLanguage",
               0x49: "mimeMediaType", 0x4A: "memberAttrName"}
TLS_VERSIONS = {0x0300: "SSL3", 0x0301: "TLS1.0", 0x0302: "TLS1.1",
                0x0303: "TLS1.2", 0x0304: "TLS1.3"}
TLS_RECORDS = {20: "change_cipher_spec", 21: "alert", 22: "handshake",
               23: "application_data", 24: "heartbeat"}
TLS_ALERTS = {0: "close_notify", 10: "unexpected_message", 20: "bad_record_mac",
              40: "handshake_failure", 42: "bad_certificate", 43: "unsupported_certificate",
              44: "certificate_revoked", 45: "certificate_expired", 46: "certificate_unknown",
              47: "illegal_parameter", 48: "unknown_ca", 49: "access_denied",
              50: "decode_error", 51: "decrypt_error", 70: "protocol_version",
              71: "insufficient_security", 80: "internal_error", 90: "user_canceled",
              109: "missing_extension", 112: "unrecognized_name", 120: "no_application_protocol"}


def tls_handshakes(data):
    """Yield safe metadata for complete ClientHello/ServerHello messages."""
    off = 0
    while off + 4 <= len(data):
        kind, size = data[off], int.from_bytes(data[off + 1:off + 4], "big")
        off += 4
        if off + size > len(data):
            return
        body = data[off:off + size]
        off += size
        if kind == 1 and len(body) >= 34:  # ClientHello
            pos = 34
            if pos >= len(body): continue
            sid_len = body[pos]; pos += 1 + sid_len
            if pos + 2 > len(body): continue
            suites_len = int.from_bytes(body[pos:pos + 2], "big"); pos += 2 + suites_len
            if pos >= len(body): continue
            comp_len = body[pos]; pos += 1 + comp_len
            if pos + 2 > len(body): continue
            ext_len = int.from_bytes(body[pos:pos + 2], "big"); pos += 2
            ext_end = min(len(body), pos + ext_len)
            sni, supported = None, []
            while pos + 4 <= ext_end:
                ext_type = int.from_bytes(body[pos:pos + 2], "big")
                length = int.from_bytes(body[pos + 2:pos + 4], "big"); pos += 4
                if pos + length > ext_end: break
                ext = body[pos:pos + length]; pos += length
                if ext_type == 0 and len(ext) >= 5:
                    cursor, list_end = 2, min(len(ext), 2 + int.from_bytes(ext[:2], "big"))
                    while cursor + 3 <= list_end:
                        name_type = ext[cursor]; nlen = int.from_bytes(ext[cursor+1:cursor+3], "big"); cursor += 3
                        if cursor + nlen > list_end: break
                        if name_type == 0:
                            sni = ext[cursor:cursor+nlen].decode("ascii", "replace")
                        cursor += nlen
                elif ext_type == 43 and length >= 3 and ext[0] == length - 1:
                    supported = [TLS_VERSIONS.get(int.from_bytes(ext[i:i+2], "big"), f"0x{int.from_bytes(ext[i:i+2], 'big'):04x}")
                                 for i in range(1, length, 2)]
            yield {"type": "ClientHello", "legacy_version": TLS_VERSIONS.get(int.from_bytes(body[:2], "big"), "unknown"),
                   "sni": sni, "supported_versions": supported}
        elif kind == 2 and len(body) >= 38:  # ServerHello
            legacy = int.from_bytes(body[:2], "big")
            pos = 34
            if pos >= len(body): continue
            sid_len = body[pos]; pos += 1 + sid_len
            if pos + 5 > len(body): continue
            pos += 3  # cipher suite + compression method
            ext_len = int.from_bytes(body[pos:pos+2], "big"); pos += 2
            ext_end = min(len(body), pos + ext_len)
            negotiated = legacy
            while pos + 4 <= ext_end:
                ext_type = int.from_bytes(body[pos:pos+2], "big")
                length = int.from_bytes(body[pos+2:pos+4], "big"); pos += 4
                if pos + length > ext_end: break
                if ext_type == 43 and length == 2:
                    negotiated = int.from_bytes(body[pos:pos+2], "big")
                pos += length
            yield {"type": "ServerHello", "version": TLS_VERSIONS.get(negotiated, f"0x{negotiated:04x}")}


def parse_tls_records(stream):
    """Parse record framing and public handshake/alert metadata, never TLS payloads."""
    records, handshakes, pos, handshake_data, incomplete = [], [], 0, bytearray(), False
    while pos < len(stream):
        if pos + 5 > len(stream):
            incomplete = True; break
        kind = stream[pos]
        version = int.from_bytes(stream[pos+1:pos+3], "big")
        length = int.from_bytes(stream[pos+3:pos+5], "big")
        if kind not in TLS_RECORDS or version not in TLS_VERSIONS or length > 18432:
            return None
        if pos + 5 + length > len(stream):
            incomplete = True
            records.append({"kind": TLS_RECORDS[kind], "version": TLS_VERSIONS[version],
                            "length": length, "captured": len(stream) - pos - 5,
                            "header": stream[pos:pos+5].hex(), "truncated": True})
            break
        content = stream[pos+5:pos+5+length]
        record = {"kind": TLS_RECORDS[kind], "version": TLS_VERSIONS[version],
                  "length": length, "header": stream[pos:pos+5].hex()}
        if kind == 21:
            record["alerts"] = [("warning" if content[i] == 1 else "fatal" if content[i] == 2 else f"level-{content[i]}",
                                 TLS_ALERTS.get(content[i+1], f"description-{content[i+1]}"))
                                for i in range(0, len(content)-1, 2)]
        records.append(record)
        if kind == 22:
            handshake_data.extend(content)
            parsed = list(tls_handshakes(handshake_data))
            # Discard only complete handshake messages. Incomplete tail remains buffered.
            consumed = 0
            while consumed + 4 <= len(handshake_data):
                msg_len = int.from_bytes(handshake_data[consumed+1:consumed+4], "big")
                if consumed + 4 + msg_len > len(handshake_data): break
                consumed += 4 + msg_len
            if parsed:
                handshakes.extend(parsed)
            if consumed:
                del handshake_data[:consumed]
        pos += 5 + length
    return records, handshakes, incomplete


def pcapng_packets(data):
    """Yield (linktype, captured packet) from validated SHB/IDB/EPB blocks."""
    pos, endian, links, resolutions = 0, None, [], []
    while pos < len(data):
        if len(data) - pos < 12:
            raise ValueError("truncated PCAPNG block header")
        magic = data[pos:pos + 4]
        if magic == b"\x0a\x0d\x0d\x0a":
            if len(data) - pos < 28:
                raise ValueError("truncated section header block")
            bom = data[pos + 8:pos + 12]
            endian = "<" if bom == b"\x4d\x3c\x2b\x1a" else ">" if bom == b"\x1a\x2b\x3c\x4d" else None
            if endian is None:
                raise ValueError("invalid PCAPNG byte-order magic")
            length = struct.unpack_from(endian + "I", data, pos + 4)[0]
            links, resolutions = [], []
        else:
            if endian is None:
                raise ValueError("PCAPNG does not begin with a section header")
            block_type, length = struct.unpack_from(endian + "II", data, pos)
        if length < 12 or length % 4 or pos + length > len(data):
            raise ValueError("invalid or truncated PCAPNG block length")
        if struct.unpack_from(endian + "I", data, pos + length - 4)[0] != length:
            raise ValueError("PCAPNG block boundary length mismatch")
        if magic == b"\x0a\x0d\x0d\x0a":
            block_type = 0x0A0D0D0A
        body = data[pos + 8:pos + length - 4]
        if block_type == 1:  # Interface Description Block
            if len(body) < 8:
                raise ValueError("short interface description block")
            links.append(struct.unpack_from(endian + "H", body)[0])
            resolution = 1e-6  # PCAPNG default: microseconds
            opt = 8
            while opt + 4 <= len(body):
                code, size = struct.unpack_from(endian + "HH", body, opt)
                opt += 4
                if code == 0:
                    break
                if opt + size > len(body):
                    raise ValueError("truncated interface option")
                if code == 9 and size == 1:
                    value = body[opt]
                    resolution = (2.0 ** -(value & 0x7f)) if value & 0x80 else (10.0 ** -value)
                opt += (size + 3) & ~3
            resolutions.append(resolution)
        elif block_type == 6:  # Enhanced Packet Block
            if len(body) < 20:
                raise ValueError("short enhanced packet block")
            iface, high, low, captured, original = struct.unpack_from(endian + "IIIII", body)
            if iface >= len(links):
                raise ValueError("packet references unknown interface")
            if captured > original or 20 + captured > len(body):
                raise ValueError("invalid enhanced packet length")
            timestamp = ((high << 32) | low) * resolutions[iface]
            yield links[iface], body[20:20 + captured], timestamp
        pos += length


def decode_packet(linktype, frame):
    if linktype == 1:  # Ethernet
        if len(frame) < 14:
            return None
        kind, off = struct.unpack_from("!H", frame, 12)[0], 14
        while kind in (0x8100, 0x88A8, 0x9100):
            if off + 4 > len(frame): return None
            kind = struct.unpack_from("!H", frame, off + 2)[0]; off += 4
        if kind != 0x0800: return None
    elif linktype == 101:  # raw IP, version nibble selects IPv4
        off = 0
    elif linktype == 228:  # DLT_IPV4
        off = 0
    else:
        return None
    if off + 20 > len(frame) or frame[off] >> 4 != 4: return None
    ihl = (frame[off] & 15) * 4
    total = struct.unpack_from("!H", frame, off + 2)[0]
    if ihl < 20 or off + total > len(frame) or off + ihl > len(frame): return None
    frag = struct.unpack_from("!H", frame, off + 6)[0]
    if frag & 0x1fff: return None
    proto = frame[off + 9]
    src, dst = str(ipaddress.IPv4Address(frame[off + 12:off + 16])), str(ipaddress.IPv4Address(frame[off + 16:off + 20]))
    payload = frame[off + ihl:off + total]
    if proto == 6 and len(payload) >= 20:
        sport, dport, seq, ack = struct.unpack_from("!HHII", payload)
        hlen = (payload[12] >> 4) * 4
        if hlen < 20 or len(payload) < hlen: return None
        flags = payload[13]
        return ("tcp", src, sport, dst, dport, seq, ack, flags, payload[hlen:])
    if proto == 17 and len(payload) >= 8:
        sport, dport, size = struct.unpack_from("!HHH", payload)
        return ("udp", src, sport, dst, dport, 0, 0, 0, payload[8:min(len(payload), size)])
    return None


def tcp_runs(segments):
    """Deduplicate retransmits, order segments, and return separate gap-free runs."""
    if not segments: return [], False, 0
    base = segments[0][0]
    ordered = sorted((((seq - base + 0x80000000) & 0xffffffff) - 0x80000000, payload)
                     for seq, payload in segments if payload)
    runs, current, end, run_start, had_gap, conflicts = [], bytearray(), None, None, False, 0
    for start, payload in ordered:
        if end is None or start > end:
            if current: runs.append(bytes(current)); had_gap = True
            current = bytearray(payload); run_start = start; end = start + len(payload)
        else:
            overlap = max(0, end - start)
            overlap = min(overlap, len(payload))
            offset = start - run_start
            if overlap and current[offset:offset + overlap] != payload[:overlap]:
                conflicts += 1
            if overlap < len(payload):
                current.extend(payload[overlap:]); end += len(payload) - overlap
    if current: runs.append(bytes(current))
    return runs, had_gap, conflicts


def ipp_info(body):
    if len(body) < 8: return None
    version = f"{body[0]}.{body[1]}"
    code, reqid = struct.unpack_from("!HI", body, 2)
    result = {"version": version, "code": code, "request_id": reqid, "attrs": []}
    off, group, collection_depth, previous_name = 8, None, 0, None
    while off < len(body):
        tag = body[off]; off += 1
        if tag == 0x03:
            if collection_depth:
                raise ValueError("IPP collection is not closed")
            result["complete"] = True
            return result
        if tag < 0x10:
            if collection_depth:
                raise ValueError("IPP group delimiter inside collection")
            group = tag; continue
        if off + 2 > len(body): break
        n = struct.unpack_from("!H", body, off)[0]; off += 2
        if off + n + 2 > len(body):
            raise ValueError("truncated IPP attribute name")
        name = body[off:off+n].decode("ascii", "replace") if n else previous_name
        off += n
        vlen = struct.unpack_from("!H", body, off)[0]; off += 2
        if off + vlen > len(body):
            raise ValueError("truncated IPP attribute value")
        value = body[off:off+vlen]
        if tag == 0x34:
            if vlen:
                raise ValueError("invalid IPP beginCollection value")
            collection_depth += 1
        elif tag == 0x37:
            if not collection_depth or n or vlen:
                raise ValueError("invalid IPP endCollection")
            collection_depth -= 1
        elif tag == 0x4A and collection_depth:
            pass  # memberAttrName value is an attribute name, not user content.
        off += vlen
        if n:
            previous_name = name
            result["attrs"].append((group, name, VALUE_TYPES.get(tag, f"tag-0x{tag:02x}")))
        if name in {"attributes-charset", "attributes-natural-language", "printer-uri",
                    "job-uri", "printer-uuid", "document-format", "document-format-default",
                    "document-format-supported", "printer-uri-supported", "printer-state-reasons"} and tag in (0x41, 0x42, 0x44, 0x45, 0x47, 0x48, 0x49):
            result.setdefault("safe_values", []).append((name, value.decode("ascii", "replace")))
        elif name in {"printer-is-accepting-jobs", "printer-state"} and tag in (0x22, 0x23):
            number = int.from_bytes(value, "big", signed=(tag == 0x23))
            if tag == 0x22 and number in (0, 1):
                shown = "true" if number else "false"
            elif name == "printer-state":
                shown = {3: "idle", 4: "processing", 5: "stopped"}.get(number, str(number))
            else:
                shown = str(number)
            result.setdefault("safe_values", []).append((name, shown))
    raise ValueError("IPP end-of-attributes delimiter is missing")


def dechunk(data):
    out, off = bytearray(), 0
    while True:
        eol = data.find(b"\r\n", off)
        if eol < 0: return None
        try: size = int(data[off:eol].split(b";", 1)[0], 16)
        except ValueError: return None
        off = eol + 2
        if size == 0:
            while True:
                trailer_end = data.find(b"\r\n", off)
                if trailer_end < 0: return None
                trailer = data[off:trailer_end]
                if not trailer:
                    return bytes(out), trailer_end + 2
                if b":" not in trailer: return None
                off = trailer_end + 2
        if off + size + 2 > len(data) or data[off+size:off+size+2] != b"\r\n": return None
        out.extend(data[off:off+size]); off += size + 2


def http_messages(stream):
    """Parse complete HTTP messages without retaining/printing arbitrary bodies."""
    off = 0
    while off < len(stream):
        end = stream.find(b"\r\n\r\n", off)
        if end < 0:
            yield None
            return
        header = stream[off:end]
        lines = header.split(b"\r\n")
        first_bytes = lines[0]
        is_request = re.fullmatch(rb"(?:POST|GET|OPTIONS|HEAD|PUT|DELETE|PRINT) [^\r\n ]+ HTTP/1\.[01]", first_bytes)
        is_response = re.fullmatch(rb"HTTP/1\.[01] [0-9]{3}(?: [^\r\n]*)?", first_bytes)
        if not (is_request or is_response):
            yield None
            return
        first = first_bytes.decode("ascii")
        headers = {}
        for line in lines[1:]:
            if b":" not in line:
                yield None
                return
            k, v = line.split(b":", 1)
            if not re.fullmatch(rb"[!#$%&'*+.^_`|~0-9A-Za-z-]+", k):
                yield None
                return
            headers[k.lower()] = v.strip()
        body_start = end + 4
        if headers.get(b"transfer-encoding", b"").lower().endswith(b"chunked"):
            parsed = dechunk(stream[body_start:])
            if parsed is None:
                yield None
                return
            body, consumed = parsed
            off = body_start + consumed
        else:
            try: length = int(headers.get(b"content-length", b"0"))
            except ValueError:
                yield None
                return
            if length < 0 or body_start + length > len(stream):
                yield None
                return
            body = stream[body_start:body_start + length]; off = body_start + length
        yield first, headers, body


def analyze(path):
    with open(path, "rb") as capture:
        data = capture.read(64 * 1024 * 1024 + 1)
    if len(data) > 64 * 1024 * 1024:
        raise ValueError("capture exceeds 64 MiB limit")
    flows, phone_mdns, all_mdns = {}, 0, 0
    packet_count = 0
    for link, frame, timestamp in pcapng_packets(data):
        packet = decode_packet(link, frame)
        if not packet: continue
        proto, src, sport, dst, dport, seq, ack, fl, payload = packet
        packet_count += 1
        if proto == "udp" and (sport == 5353 or dport == 5353):
            all_mdns += 1
            if src == PHONE or dst == PHONE: phone_mdns += 1
        if proto != "tcp" or PHONE not in (src, dst) or not ({sport, dport} & PORTS): continue
        endpoints = tuple(sorted(((src, sport), (dst, dport))))
        signature = (src, sport, seq, ack, fl, payload)
        flows.setdefault(endpoints, {}).setdefault(signature, (timestamp, src, sport, dst, dport, seq, ack, fl, payload))
    print(f"Packets parsed: {packet_count}; UDP/5353 packets: all={all_mdns}, phone={phone_mdns}")
    transactions = {}
    for endpoints, deduped in flows.items():
        ordered = sorted(deduped.values(), key=lambda event: event[0])
        epochs, current, syn_marker, terminal = [], [], None, False
        for event in ordered:
            timestamp, src, sport, dst, dport, seq, ack, fl, payload = event
            marker = (src, sport, seq)
            if fl & 2 and not fl & 16 and current and (terminal or syn_marker != marker):
                epochs.append(current)
                current, terminal, syn_marker = [], False, None
            if fl & 2 and not fl & 16 and syn_marker is None:
                syn_marker = marker
            current.append(event)
            if fl & 5:
                terminal = True
        if current:
            epochs.append(current)
        for epoch_no, epoch in enumerate(epochs, 1):
            directional, flag_counts = {}, {"SYN": 0, "FIN": 0, "RST": 0}
            for timestamp, src, sport, dst, dport, seq, ack, fl, payload in epoch:
                for bit, label in ((2, "SYN"), (1, "FIN"), (4, "RST")):
                    if fl & bit:
                        flag_counts[label] += 1
                directional.setdefault((src, sport, dst, dport), []).append(
                    ((seq + (1 if fl & 2 else 0)) & 0xffffffff, payload, timestamp, fl))
            for key, segments in directional.items():
                runs, gap, conflicts = tcp_runs([(seq, payload) for seq, payload, _, _ in segments])
                src, sport, dst, dport = key
                direction = f"{src}:{sport} -> {dst}:{dport}"
                capture_time = min((ts for _, payload, ts, _ in segments if payload), default=None)
                when = datetime.fromtimestamp(capture_time, timezone.utc).astimezone().isoformat(timespec="milliseconds") if capture_time is not None else "unknown"
                suffix = (" [GAP: streams kept separate]" if gap else "") + (f" [CONFLICTING OVERLAP SEGMENTS={conflicts}]" if conflicts else "")
                direction_flags = {label: sum(1 for _, _, _, fl in segments if fl & bit)
                                   for bit, label in ((2, "SYN"), (1, "FIN"), (4, "RST"))}
                print(f"TCP connection={epoch_no} {direction} time={when} flags={direction_flags}{suffix}")
                for run in runs:
                    tls = parse_tls_records(run) if run and run[0] in TLS_RECORDS else None
                    if tls:
                        records, handshakes, incomplete = tls
                        for record in records:
                            text = (f"  TLS record={record['kind']} version={record['version']} length={record['length']} "
                                    f"header={record['header']}")
                            if record.get("truncated"):
                                text += f" captured={record['captured']} truncated=true"
                            print(text)
                            for level, description in record.get("alerts", []):
                                print(f"    TLS alert level={level} description={description}")
                        for hello in handshakes:
                            if hello["type"] == "ClientHello":
                                print(f"    TLS ClientHello legacy-version={hello['legacy_version']} SNI={hello['sni'] or 'absent'} supported={','.join(hello['supported_versions']) or 'absent'}")
                            else:
                                print(f"    TLS ServerHello negotiated-version={hello['version']}")
                        print(f"  TLS encrypted application-data records={sum(r['kind'] == 'application_data' for r in records)} alerts-visible={sum(len(r.get('alerts', [])) for r in records)} handshake-record-incomplete={incomplete}")
                        continue
                    for message in http_messages(run):
                        if message is None:
                            print("  HTTP fragment/unrecognized: stopped at unvalidated boundary")
                            break
                        start, headers, body = message
                        print(f"  HTTP {start}")
                        for name, value in headers.items():
                            print(f"    {name.decode('ascii', 'replace')}: {value.decode('ascii', 'replace')}")
                        content_type = headers.get(b"content-type", b"").split(b";", 1)[0].strip().lower()
                        ipp = None
                        if content_type == b"application/ipp":
                            try:
                                ipp = ipp_info(body)
                            except ValueError as exc:
                                print(f"    IPP INVALID: {exc}")
                        if not ipp:
                            continue
                        op = IPP_OPS.get(ipp["code"], f"0x{ipp['code']:04x}") if start.startswith("POST ") else None
                        status = IPP_STATUS.get(ipp["code"], f"0x{ipp['code']:04x}") if start.startswith("HTTP/") else None
                        print(f"    IPP version={ipp['version']} " + (f"operation={op}" if op else f"status={status}") + f" request-id={ipp['request_id']}")
                        print(f"    IPP attributes-complete={ipp.get('complete', False)}")
                        for name, value in ipp.get("safe_values", []):
                            print(f"      {name}={value}")
                        for group, name, kind in ipp["attrs"]:
                            print(f"      attribute group=0x{(group or 0):02x} name={name} type={kind}")
                        side = "request" if start.startswith("POST ") else "response"
                        txkey = (endpoints[0], endpoints[1], ipp["request_id"])
                        transactions.setdefault(txkey, {})[side] = (op or status, direction)
    for txkey, pair in sorted(transactions.items()):
        reqid = txkey[2]
        req, resp = pair.get("request"), pair.get("response")
        if req or resp:
            print(f"IPP correlation request-id={reqid}: request={req[0] if req else 'missing'} response={resp[0] if resp else 'missing'}")


def make_block(kind, body, endian="<"):
    length = 12 + len(body)
    return struct.pack(endian + "II", kind, length) + body + struct.pack(endian + "I", length)


def make_packet(payload, seq=100, flags=0x18, src="192.168.3.197", dst="192.168.3.10", sport=50000, dport=631):
    tcp = struct.pack("!HHIIHHHH", sport, dport, seq, 0, (5 << 12) | flags, 65535, 0, 0) + payload
    ip = bytearray(struct.pack("!BBHHHBBH4s4s", 0x45, 0, 20 + len(tcp), 1, 0, 64, 6, 0,
                               ipaddress.IPv4Address(src).packed, ipaddress.IPv4Address(dst).packed))
    return b"\0" * 12 + b"\x08\x00" + bytes(ip) + tcp


def self_test():
    endian = "<"
    shb = make_block(0x0A0D0D0A, struct.pack("<IHHq", 0x1A2B3C4D, 1, 0, -1))
    idb = make_block(1, struct.pack("<HHI", 1, 0, 65535))
    ipp = b"\x02\x00\x00\x0b\x00\x00\x00\x07" + b"\x01\x47\x00\x12attributes-charset\x00\x05utf-8\x03"
    ipp += b"\r\n0\r\n\r\n"  # Marker-like bytes inside payload must not end the chunk.
    http = b"POST /ipp/print HTTP/1.1\r\nHost: printer\r\nTransfer-Encoding: chunked\r\n\r\n"
    chunked = f"{len(ipp):x}\r\n".encode() + ipp + b"\r\n0\r\n\r\n"
    wire = http + chunked
    # Split and retransmit an overlapping section; sequence reassembly must dedupe.
    packets = [make_packet(wire[30:], 130), make_packet(wire[:35], 100), make_packet(wire[30:55], 130)]
    blocks = b"".join(make_block(6, struct.pack("<IIIII", 0, 0, 1_000_000 + i, len(pkt), len(pkt)) + pkt + b"\0" * ((-len(pkt)) % 4)) for i, pkt in enumerate(packets))
    capture = shb + idb + blocks
    parsed = list(pcapng_packets(capture))
    assert len(parsed) == 3 and all(link == 1 for link, _, _ in parsed)
    assert parsed[0][2] == 1.0
    decoded = [decode_packet(link, pkt) for link, pkt, _ in parsed]
    runs, gap, conflicts = tcp_runs([(p[5] + (1 if p[7] & 2 else 0), p[8]) for p in decoded])
    assert not gap and not conflicts and runs == [wire], "TCP overlap/retransmit reassembly failed"
    msgs = list(http_messages(runs[0]))
    assert len(msgs) == 1 and msgs[0][2] == ipp
    assert list(http_messages(b"HTTP 0\r\nContent-Length: 4\r\n\r\nIPP!")) == [None]
    info = ipp_info(msgs[0][2])
    assert info and info["request_id"] == 7 and info["code"] == 0x000b and IPP_OPS[info["code"]] == "Get-Printer-Attributes"
    assert info["attrs"][0][1:] == ("attributes-charset", "charset")
    assert info["complete"] and info["safe_values"] == [("attributes-charset", "utf-8")]
    collection = b"\x02\x00\x00\x02\x00\x00\x00\x08\x01\x34\x00\x01x\x00\x00" + b"\x4a\x00\x00\x00\x03foo" + b"\x21\x00\x00\x00\x04\0\0\0\x01" + b"\x37\x00\x00\x00\x00\x03"
    assert ipp_info(collection)["complete"]
    for invalid in (collection[:-1], collection[:-6] + b"\x03"):
        try:
            ipp_info(invalid)
            raise AssertionError("truncated/invalid IPP collection accepted")
        except ValueError:
            pass
    try:
        list(pcapng_packets(capture[:-1]))
        raise AssertionError("truncated PCAPNG accepted")
    except ValueError:
        pass
    # A missing sequence range must remain two runs, never be concatenated.
    runs, gap, conflicts = tcp_runs([(10, b"abc"), (20, b"xyz")])
    assert gap and not conflicts and runs == [b"abc", b"xyz"]
    runs, gap, conflicts = tcp_runs([(10, b"abc"), (10, b"axc")])
    assert not gap and conflicts == 1 and runs == [b"abc"]
    sni = b"printer.local"
    sni_data = (len(sni) + 3).to_bytes(2, "big") + b"\x00" + len(sni).to_bytes(2, "big") + sni
    exts = b"\x00\x00" + len(sni_data).to_bytes(2, "big") + sni_data
    ch_body = b"\x03\x03" + b"\0" * 32 + b"\0\x00\x02\x13\x01\x01\x00" + len(exts).to_bytes(2, "big") + exts
    ch = b"\x01" + len(ch_body).to_bytes(3, "big") + ch_body
    sh_ext = b"\x00\x2b\x00\x02\x03\x04"
    sh_body = b"\x03\x03" + b"\0" * 32 + b"\0\x13\x01\0" + len(sh_ext).to_bytes(2, "big") + sh_ext
    sh = b"\x02" + len(sh_body).to_bytes(3, "big") + sh_body
    tls_wire = b"\x16\x03\x01" + len(ch).to_bytes(2, "big") + ch
    tls_wire += b"\x16\x03\x03" + len(sh).to_bytes(2, "big") + sh
    tls_wire += b"\x17\x03\x03\x00\x03abc\x15\x03\x03\x00\x02\x02\x28"
    records, hellos, incomplete = parse_tls_records(tls_wire)
    assert not incomplete and [h["type"] for h in hellos] == ["ClientHello", "ServerHello"]
    assert hellos[0]["sni"] == "printer.local" and hellos[1]["version"] == "TLS1.3"
    assert sum(r["kind"] == "application_data" for r in records) == 1
    assert records[-1]["alerts"] == [("fatal", "handshake_failure")]
    print("self-test: ok (PCAPNG bounds, chunked IPP, retransmit dedupe, TCP gap)")


def main():
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("capture", nargs="?", help="PCAPNG file produced by pktmon etl2pcap")
    parser.add_argument("--self-test", action="store_true", help="run a synthetic parser check")
    args = parser.parse_args()
    if args.self_test:
        self_test(); return 0
    if not args.capture:
        parser.error("capture is required unless --self-test is used")
    try:
        analyze(args.capture)
    except (OSError, ValueError) as exc:
        print(f"error: {exc}", file=sys.stderr); return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
