"""Independent, loopback-only EtherNet/IP Class 1 test fixture."""
import argparse
import json
import math
from pathlib import Path
import socket
import struct
import threading
import time

IP = "127.0.0.1"
SESSION = 0xABCD0123
OT_ID = 0x1234ABCD


def recv_exact(sock, length):
    data = bytearray()
    while len(data) < length:
        chunk = sock.recv(length - len(data))
        if not chunk:
            raise EOFError
        data.extend(chunk)
    return data


def rr_items(raw):
    assert raw[:4] == bytes(4)
    count = struct.unpack_from("<H", raw, 6)[0]
    items = {}
    offset = 8
    for _ in range(count):
        code, length = struct.unpack_from("<HH", raw, offset)
        offset += 4
        items[code] = raw[offset:offset + length]
        offset += length
    assert offset == len(raw)
    return items


def rr_reply(cip, *extra):
    items = [(0, b""), (0xB2, cip), *extra]
    return struct.pack("<IHH", 0, 0, len(items)) + b"".join(
        struct.pack("<HH", code, len(data)) + data for code, data in items
    )


def sockaddr(port):
    return struct.pack(">HH4s8x", 2, port, socket.inet_aton(IP))


def io_packet(connection, seq, data):
    return struct.pack("<HHHIIHHH", 2, 0x8002, 8, connection, seq,
                       0xB1, 2 + len(data), seq & 0xFFFF) + data


def spacing(stamps):
    gaps = sorted((later - earlier) // 1000 for earlier, later in zip(stamps, stamps[1:]))
    if not gaps:
        return {}
    return {f"p{p}_us": gaps[math.ceil(p / 100 * len(gaps)) - 1] for p in (50, 95, 99)} | {
        "max_us": gaps[-1], "intervals": len(gaps),
    }


def udp_loop(udp, endpoint, connection, api, mode, stop, report):
    sent, received = [], []
    output_values = set()
    owner = mode == "owner"
    prior_sequence = None
    missed = invalid = skipped = 0
    seq = 0
    period = api * 1000
    deadline = None
    udp.setblocking(False)
    while not stop.is_set():
        while True:
            try:
                packet, peer = udp.recvfrom(2048)
                stamp = time.perf_counter_ns()
            except BlockingIOError:
                break
            if peer != endpoint or len(packet) != (26 if owner else 20):
                invalid += 1
                continue
            count, addr_type, addr_size, cid, sequence, dtype, size, cip = struct.unpack_from(
                "<HHHIIHHH", packet
            )
            if (count, addr_type, addr_size, cid, dtype, size, cip) != (
                2, 0x8002, 8, OT_ID, 0xB1, (8 if owner else 2), sequence & 0xFFFF
            ):
                invalid += 1
                continue
            if owner:
                assert packet[20:24] == b"\x01\x00\x00\x00"
                output_values.add(packet[24:].hex())
            if prior_sequence is not None:
                missed += (sequence - prior_sequence - 1) & 0xFFFFFFFF
            prior_sequence = sequence
            received.append(stamp)
            if deadline is None:
                deadline = stamp
        now = time.perf_counter_ns()
        if deadline is not None and now >= deadline:
            slots = (now - deadline) // period + 1
            skipped += slots - 1
            deadline += slots * period
            seq = (seq + 1) & 0xFFFFFFFF
            if not (mode == "faults" and seq == 60) and not (mode == "stall" and seq > 75):
                value = struct.pack("<I", seq // 16)
                packet = io_packet(connection, seq, (bytes((1, 0, 0, 0)) if owner else b"") + value)
                udp.sendto(packet, endpoint)
                sent.append(time.perf_counter_ns())
                if mode == "faults" and seq == 80:
                    udp.sendto(packet, endpoint)
                    udp.sendto(io_packet(connection, 60, value), endpoint)
                    udp.sendto(io_packet(connection, seq, value[:-1]), endpoint)
        if deadline is None:
            time.sleep(0.00015)
        elif deadline - now > 250_000:
            time.sleep(min((deadline - now - 160_000) / 1e9, 0.0004))
    report.update({
        "ot_received": len(received), "ot_sequence_gaps": missed, "ot_invalid": invalid,
        "ot_receive_spacing": spacing(received), "to_sent": len(sent),
        "to_send_spacing": spacing(sent), "to_skipped_deadline_slots": skipped, "output_values": sorted(output_values),
    })


def serve(tcp, udp, mode, report):
    stop = threading.Event()
    io = None
    owner = mode == "owner"
    path = bytes.fromhex("20 04 24 01 2C 64 2C 65" if owner else "20 04 24 01 2C FE 2C 65")
    try:
        while True:
            cmd, length, handle, status, ctx, options = struct.unpack(
                "<HHII8sI", recv_exact(tcp, 24)
            )
            request = recv_exact(tcp, length)
            assert status == options == 0
            if cmd == 0x65:
                assert handle == 0 and request == b"\x01\x00\x00\x00"
                response = request
            elif cmd == 0x66:
                assert handle == SESSION
                break
            elif cmd == 0x6F:
                assert handle == SESSION
                items = rr_items(request)
                service, words, cls, object_class, inst, instance = items[0xB2][:6]
                assert (words, cls, inst, instance) == (2, 0x20, 0x24, 1)
                body = items[0xB2][6:]
                if service == 1 and object_class == 1:
                    name = b"Loopback Adapter"
                    reply = struct.pack("<HHHBBHIB", 0, 12, 1, 1, 0, 0x0060, 0x12345678, len(name)) + name
                    response = rr_reply(b"\x81\0\0\0" + reply)
                elif service == 0x54 and object_class == 6:
                    assert io is None
                    _, to, serial, vendor, origin = struct.unpack_from("<IIHHI", body, 2)
                    ot_rpi, ot_params, to_rpi, to_params, transport, path_words = struct.unpack_from(
                        "<IHIHBB", body, 22
                    )
                    assert transport == 1
                    assert len(body[36:]) == path_words * 2
                    assert body[36:] == path + (bytes.fromhex("80 02 AA BB CC 00") if owner else b"")
                    assert ot_params & 0xE3FF == (0x4008 if owner else 0x4002)
                    assert to_params & 0xE3FF == (0x400A if owner else 0x4006)
                    family, port, address = struct.unpack(">HH4s8x", items[0x8001])
                    assert family == 2 and socket.inet_ntoa(address) == IP and port != 0
                    report.update(ot_requested_us=ot_rpi, to_requested_us=to_rpi)
                    if mode == "reject":
                        response = rr_reply(bytes.fromhex("D4 00 01 01 09 01"))
                    else:
                        ot_api, to_api = (1250, 1500) if mode == "different-api" else (ot_rpi, to_rpi)
                        reply = struct.pack("<IIHHIIIBB", OT_ID, to, serial, vendor, origin,
                                            ot_api, to_api, 0, 0)
                        response = rr_reply(b"\xD4\0\0\0" + reply,
                                            (0x8000, sockaddr(udp.getsockname()[1])))
                        io = threading.Thread(target=udp_loop,
                                              args=(udp, (IP, port), to, to_api, mode, stop, report))
                        io.start()
                elif service == 0x4E and object_class == 6:
                    assert struct.unpack_from("<HHI", body, 2) == (serial, vendor, origin)
                    assert body[12:] == path
                    report["forward_close"] = True
                    stop.set()
                    response = rr_reply(b"\xCE\0\0\0" + body[2:10] + b"\0\0")
                else:
                    raise AssertionError(f"Unexpected CIP request {service:#x}/{object_class:#x}")
            else:
                raise AssertionError(f"Unexpected command {cmd:#x}")
            tcp.sendall(struct.pack("<HHII8sI", cmd, len(response), SESSION, 0, ctx, 0) + response)
    finally:
        stop.set()
        if io is not None:
            io.join()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("report", type=Path)
    parser.add_argument("--mode", choices=("normal", "faults", "stall", "reject", "different-api", "owner", "reconnect"), default="normal")
    args = parser.parse_args()
    report = {"mode": args.mode, "forward_close": False}
    with socket.socket() as listener:
        listener.bind((IP, 44818))
        listener.listen(1)
        listener.settimeout(30)
        print("Ready on loopback", flush=True)
        for _ in range(2 if args.mode == "reconnect" else 1):
            conn, _ = listener.accept()
            with conn, socket.socket(type=socket.SOCK_DGRAM) as udp:
                conn.settimeout(20)
                udp.bind((IP, 0))
                serve(conn, udp, args.mode, report)
    args.report.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
