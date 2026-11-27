#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${REPO_ROOT}"

if [[ ! -f "dist/wmos.iso" || ! -f "dist/wmos.img" ]]; then
    echo "ERROR: dist/wmos.iso or dist/wmos.img not found. Run ./scripts/build-images.sh first." >&2
    exit 1
fi

rm -f dist/qemu-bios.log dist/qemu-uefi.log dist/qemu-uefi-reboot.log dist/iot-mock.log dist/wmos-persist-test.img dist/boot-preview.ppm dist/boot-preview.png /tmp/wmos-mon.sock /tmp/wmos-mcu.sock /tmp/wmos-mcu.ready

echo "=== [1/3] Verifying dist/wmos.iso in QEMU Legacy BIOS mode ==="
set +e
timeout 12s qemu-system-x86_64 \
    -M q35 \
    -m 256M \
    -cdrom dist/wmos.iso \
    -boot d \
    -display none \
    -serial file:dist/qemu-bios.log \
    -no-reboot \
    -no-shutdown
bios_rc=$?
set -e

if [[ ${bios_rc} -ne 0 && ${bios_rc} -ne 124 ]]; then
    echo "ERROR: QEMU BIOS exited unexpectedly with code ${bios_rc}" >&2
    if [[ -f dist/qemu-bios.log ]]; then
        cat dist/qemu-bios.log >&2
    fi
    exit 1
fi

echo "=== [2/3] Verifying dist/wmos.iso + FAT32 + e1000 + IoT Hub (HTTP/MQTT/UDP/COM2 MCU) in QEMU UEFI (OVMF) Boot #1 ==="
OVMF_CANDIDATES=(
    "/usr/share/OVMF/OVMF_CODE_4M.fd"
    "/usr/share/OVMF/OVMF_CODE.fd"
    "/usr/share/ovmf/OVMF.fd"
    "/usr/share/edk2/ovmf/OVMF_CODE.fd"
)

OVMF_CODE=""
for candidate in "${OVMF_CANDIDATES[@]}"; do
    if [[ -f "${candidate}" ]]; then
        OVMF_CODE="${candidate}"
        break
    fi
done

if [[ -z "${OVMF_CODE}" ]]; then
    echo "ERROR: No OVMF firmware found among: ${OVMF_CANDIDATES[*]}" >&2
    exit 1
fi
echo "Using OVMF firmware: ${OVMF_CODE}"

cp dist/wmos.img dist/wmos-persist-test.img

QEMU_PID=""
MOCK_PID=""
stop_mock() {
    if [[ -n "${MOCK_PID}" ]] && kill -0 "${MOCK_PID}" 2>/dev/null; then
        kill "${MOCK_PID}" 2>/dev/null || true
        wait "${MOCK_PID}" 2>/dev/null || true
    fi
    MOCK_PID=""
    rm -f /tmp/wmos-mcu.sock /tmp/wmos-mcu.ready
}
stop_qemu() {
    if [[ -n "${QEMU_PID}" ]] && kill -0 "${QEMU_PID}" 2>/dev/null; then
        kill "${QEMU_PID}" 2>/dev/null || true
        wait "${QEMU_PID}" 2>/dev/null || true
    fi
    QEMU_PID=""
    rm -f /tmp/wmos-mon.sock
}
cleanup_all() {
    stop_qemu
    stop_mock
    rm -f dist/wmos-persist-test.img
}
trap cleanup_all EXIT

python3 - <<'PY' &
import os
import socket
import threading
import time

log_path = "dist/iot-mock.log"
log_lock = threading.Lock()

def log_event(msg: str) -> None:
    with log_lock:
        with open(log_path, "a", encoding="utf-8") as f:
            f.write(msg + "\n")
            f.flush()

def run_http_server() -> None:
    srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind(("127.0.0.1", 18080))
    srv.listen(8)
    while True:
        conn, _ = srv.accept()
        with conn:
            conn.settimeout(1.5)
            data = b""
            try:
                while b"\r\n\r\n" not in data and len(data) < 4096:
                    chunk = conn.recv(1024)
                    if not chunk:
                        break
                    data += chunk
            except OSError:
                pass
            first_line = data.split(b"\r\n", 1)[0].decode("utf-8", errors="replace").strip()
            if first_line:
                log_event(f"HTTP {first_line}")
            resp = (
                b"HTTP/1.0 200 OK\r\n"
                b"Content-Type: application/json\r\n\r\n"
                b'{"lamp":"on","temp":24}\n'
            )
            try:
                conn.sendall(resp)
            except OSError:
                pass

def read_mqtt_packet(conn: socket.socket, buf: bytearray):
    while len(buf) < 2:
        chunk = conn.recv(1024)
        if not chunk:
            return None
        buf.extend(chunk)
    pkt_type = buf[0]
    mult = 1
    rem_len = 0
    idx = 1
    while True:
        while idx >= len(buf):
            chunk = conn.recv(1024)
            if not chunk:
                return None
            buf.extend(chunk)
        digit = buf[idx]
        idx += 1
        rem_len += (digit & 0x7F) * mult
        if (digit & 0x80) == 0:
            break
        mult *= 128
    total = idx + rem_len
    while len(buf) < total:
        chunk = conn.recv(1024)
        if not chunk:
            return None
        buf.extend(chunk)
    body = bytes(buf[idx:total])
    del buf[:total]
    return pkt_type, body

def run_mqtt_server() -> None:
    srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind(("127.0.0.1", 11883))
    srv.listen(8)
    while True:
        conn, _ = srv.accept()
        with conn:
            conn.settimeout(2.0)
            buf = bytearray()
            try:
                pkt1 = read_mqtt_packet(conn, buf)
                if pkt1 and (pkt1[0] & 0xF0) == 0x10:
                    conn.sendall(b"\x20\x02\x00\x00")
                    while True:
                        pkt = read_mqtt_packet(conn, buf)
                        if pkt is None:
                            break
                        ptype, body = pkt
                        if (ptype & 0xF0) == 0x30 and len(body) >= 2:
                            tlen = (body[0] << 8) | body[1]
                            topic = body[2:2 + tlen].decode("utf-8", errors="replace")
                            payload_off = 2 + tlen
                            if (ptype & 0x06) != 0 and payload_off + 2 <= len(body):
                                payload_off += 2
                            payload = body[payload_off:].decode("utf-8", errors="replace")
                            log_event(f"MQTT {topic}={payload}")
                        elif (ptype & 0xF0) == 0xE0:
                            break
            except OSError:
                pass

def run_udp_server() -> None:
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    sock.bind(("127.0.0.1", 18888))
    while True:
        data, addr = sock.recvfrom(2048)
        msg = data.decode("utf-8", errors="replace").strip()
        log_event(f"UDP {msg}")
        sock.sendto(b'{"dev":"plug","state":"on"}', addr)

def run_mcu_client() -> None:
    mcu_sock = "/tmp/wmos-mcu.sock"
    uefi_log = "dist/qemu-uefi.log"
    ready_flag = "/tmp/wmos-mcu.ready"
    kernel_ready = False
    for _ in range(400):
        if os.path.exists(uefi_log) and os.path.exists(mcu_sock):
            try:
                with open(uefi_log, "r", encoding="utf-8", errors="replace") as lf:
                    if "[wmos] ready" in lf.read():
                        kernel_ready = True
                        break
            except OSError:
                pass
        time.sleep(0.05)
    if not kernel_ready:
        return
    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
            s.connect(mcu_sock)
            with open(ready_flag, "w", encoding="utf-8") as rf:
                rf.write("ok\n")
            buf = b""
            while True:
                chunk = s.recv(1024)
                if not chunk:
                    break
                buf += chunk
                while b"\n" in buf:
                    raw_line, buf = buf.split(b"\n", 1)
                    line = raw_line.decode("utf-8", errors="replace").strip()
                    if line and "\x1b" not in line:
                        log_event(f"MCU {line}")
                        s.sendall(b"TEMP=24C LED=ON\n")
    except OSError:
        pass

for fn in (run_http_server, run_mqtt_server, run_udp_server, run_mcu_client):
    t = threading.Thread(target=fn, daemon=True)
    t.start()

while True:
    time.sleep(1)
PY
MOCK_PID=$!
sleep 0.2

qemu-system-x86_64 \
    -M q35 \
    -m 256M \
    -drive "if=pflash,format=raw,readonly=on,file=${OVMF_CODE}" \
    -cdrom dist/wmos.iso \
    -drive "file=dist/wmos-persist-test.img,format=raw" \
    -netdev user,id=net0 \
    -device e1000,netdev=net0 \
    -vga std \
    -display none \
    -serial file:dist/qemu-uefi.log \
    -serial unix:/tmp/wmos-mcu.sock,server,nowait \
    -monitor unix:/tmp/wmos-mon.sock,server,nowait \
    -no-reboot \
    -no-shutdown &
QEMU_PID=$!

ready_found=0
for _ in $(seq 1 60); do
    if [[ -f dist/qemu-uefi.log ]] && grep -qF "[wmos] ready" dist/qemu-uefi.log; then
        ready_found=1
        break
    fi
    if ! kill -0 "${QEMU_PID}" 2>/dev/null; then
        break
    fi
    sleep 0.25
done

if [[ ${ready_found} -eq 1 ]]; then
    sleep 0.3
    python3 - <<'PY' || true
import os
import socket
import time

sock_path = "/tmp/wmos-mon.sock"
uefi_log_path = "dist/qemu-uefi.log"

def recv_until_prompt(sock: socket.socket, timeout: float = 2.0) -> None:
    deadline = time.monotonic() + timeout
    buf = b""
    while time.monotonic() < deadline:
        try:
            chunk = sock.recv(4096)
            if not chunk:
                break
            buf += chunk
            if b"(qemu)" in buf:
                break
        except OSError:
            break

def send_mon(sock: socket.socket, cmd: bytes) -> None:
    sock.sendall(cmd + b"\n")
    recv_until_prompt(sock, timeout=2.0)

def map_key(ch: str) -> bytes:
    if "a" <= ch <= "z" or "0" <= ch <= "9":
        return ch.encode("ascii")
    if ch == " ":
        return b"spc"
    if ch == ".":
        return b"dot"
    if ch == "/":
        return b"slash"
    if ch == "-":
        return b"minus"
    raise ValueError(f"Unsupported key character: {ch!r}")

def send_command(sock: socket.socket, text: str) -> None:
    for ch in text:
        send_mon(sock, b"sendkey " + map_key(ch))
        time.sleep(0.07)
    send_mon(sock, b"sendkey ret")
    time.sleep(0.07)

if os.path.exists(sock_path):
    for _ in range(40):
        if os.path.exists("/tmp/wmos-mcu.ready"):
            break
        time.sleep(0.05)
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
        s.settimeout(2.0)
        s.connect(sock_path)
        time.sleep(0.2)
        recv_until_prompt(s, timeout=1.0)

        send_mon(s, b"mouse_move 40 25")
        time.sleep(0.08)

        for cmd in (
            "lspci",
            "ps",
            "disks",
            "cat /disk/boot/limine/limine.conf",
            "write demo.txt hello",
            "cat demo.txt",
            "write /disk/persist.txt hello-v4",
            "ifconfig",
            "dhcp",
            "http 10.0.2.2 18080 /iot/lamp/on",
            "mqtt 10.0.2.2 11883 home/lamp on",
            "iot toggle plug",
            "mcu temp",
            "ping 10.0.2.2",
            "exec /disk/bin/hello.elf",
            "open iot",
        ):
            send_command(s, cmd)

        deadline = time.monotonic() + 12.0
        while time.monotonic() < deadline:
            if os.path.exists(uefi_log_path):
                with open(uefi_log_path, "r", encoding="utf-8", errors="replace") as lf:
                    content = lf.read()
                    if (
                        "[wmos] fs disk write disk/persist.txt ok" in content
                        and "[wmos] dhcp ok ip=10.0.2.15" in content
                        and "[wmos] http ok 10.0.2.2:18080/iot/lamp/on" in content
                        and "[wmos] mqtt ok home/lamp=on" in content
                        and "[wmos] iot toggle plug ok" in content
                        and "[wmos] mcu ok TEMP=24C" in content
                        and "[wmos] ping 10.0.2.2 ok" in content
                        and "[wmos] user elf exit=0" in content
                        and "[wmos] cmd: open iot" in content
                    ):
                        break
            time.sleep(0.1)

        time.sleep(0.25)
        send_mon(s, b"screendump dist/boot-preview.ppm")
PY
fi

stop_qemu
stop_mock

if [[ -f dist/boot-preview.ppm ]]; then
    python3 - <<'PY' || true
import struct
import zlib

ppm_path = "dist/boot-preview.ppm"
png_path = "dist/boot-preview.png"

with open(ppm_path, "rb") as f:
    data = f.read()

pos = 0
n = len(data)

def next_token():
    global pos
    while pos < n:
        b = data[pos:pos + 1]
        if b in (b" ", b"\t", b"\r", b"\n"):
            pos += 1
        elif b == b"#":
            while pos < n and data[pos:pos + 1] != b"\n":
                pos += 1
        else:
            break
    start = pos
    while pos < n and data[pos:pos + 1] not in (b" ", b"\t", b"\r", b"\n", b"#"):
        pos += 1
    return data[start:pos].decode("ascii")

magic = next_token()
if magic != "P6":
    raise ValueError(f"Unsupported PPM format: {magic}")

width = int(next_token())
height = int(next_token())
maxval = int(next_token())
if pos < n and data[pos:pos + 1] in (b" ", b"\t", b"\r", b"\n"):
    pos += 1

rgb = data[pos:]
expected_len = width * height * 3
if len(rgb) < expected_len:
    raise ValueError(f"Truncated PPM payload: got {len(rgb)}, expected {expected_len}")
rgb = rgb[:expected_len]

if maxval != 255:
    rgb = bytes((b * 255) // maxval for b in rgb)

row_stride = width * 3
scanlines = bytearray()
for y in range(height):
    scanlines.append(0)
    start = y * row_stride
    scanlines.extend(rgb[start:start + row_stride])

def png_chunk(chunk_type: bytes, payload: bytes) -> bytes:
    return (
        struct.pack(">I", len(payload))
        + chunk_type
        + payload
        + struct.pack(">I", zlib.crc32(chunk_type + payload) & 0xFFFFFFFF)
    )

ihdr = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
idat = zlib.compress(bytes(scanlines), level=6)

with open(png_path, "wb") as out:
    out.write(b"\x89PNG\r\n\x1a\n")
    out.write(png_chunk(b"IHDR", ihdr))
    out.write(png_chunk(b"IDAT", idat))
    out.write(png_chunk(b"IEND", b""))

print(f"Converted {ppm_path} ({width}x{height}) -> {png_path}")
PY
fi

assert_log() {
    local log_file="$1"
    local mode="$2"

    if [[ ! -f "${log_file}" ]]; then
        echo "ERROR: ${mode} serial log missing: ${log_file}" >&2
        exit 1
    fi

    echo "--- ${mode} serial log (${log_file}) ---"
    cat "${log_file}"
    echo "--- end ${mode} serial log ---"

    if grep -qF "[wmos] PANIC" "${log_file}"; then
        echo "ERROR: ${mode} log contains '[wmos] PANIC'" >&2
        exit 1
    fi
    if grep -qF "[wmos] EXCEPTION" "${log_file}"; then
        echo "ERROR: ${mode} log contains '[wmos] EXCEPTION'" >&2
        exit 1
    fi

    local required_markers=(
        "[wmos] boot ok"
        "[wmos] gdt+tss ok"
        "[wmos] idt+pic ok"
        "[wmos] syscall+ring3 ok"
        "[wmos] pmm ok"
        "[wmos] vmm self-test ok"
        "[wmos] heap ok"
        "[wmos] heap self-test ok"
        "[wmos] rtc ok"
        "[wmos] fs ok"
        "[wmos] pci ok"
        "[wmos] ahci ok"
        "[wmos] sched ok"
        "[wmos] com2 mcu uart ok"
        "[wmos] iot hub ok"
        "[wmos] pit+ps2 ok"
        "[wmos] fb="
        "[wmos] fb double-buffer ok"
        "[wmos] desktop rendered"
        "[wmos] ready"
    )
    for marker in "${required_markers[@]}"; do
        if ! grep -qF "${marker}" "${log_file}"; then
            echo "ERROR: ${mode} log missing required marker: ${marker}" >&2
            exit 1
        fi
    done
}

assert_log "dist/qemu-bios.log" "BIOS"
assert_log "dist/qemu-uefi.log" "UEFI"

for uefi_marker in \
    "[wmos] ahci ok disks=1" \
    "[wmos] disk fat32 ok" \
    "[wmos] e1000 ok mac=" \
    "[wmos] net ok ip=10.0.2.15" \
    "[wmos] mouse active" \
    "[wmos] cmd: lspci" \
    "[wmos] cmd: ps" \
    "[wmos] cmd: disks" \
    "[wmos] cmd: cat /disk/boot/limine/limine.conf" \
    "[wmos] fs cat disk/boot/limine/limine.conf ok" \
    "[wmos] cmd: write demo.txt hello" \
    "[wmos] fs cat demo.txt ok" \
    "[wmos] cmd: write /disk/persist.txt hello-v4" \
    "[wmos] fs disk write disk/persist.txt ok" \
    "[wmos] cmd: ifconfig" \
    "[wmos] cmd: dhcp" \
    "[wmos] dhcp ok ip=10.0.2.15" \
    "[wmos] http ok 10.0.2.2:18080/iot/lamp/on" \
    "[wmos] mqtt ok home/lamp=on" \
    "[wmos] iot toggle plug ok" \
    "[wmos] mcu ok TEMP=24C" \
    "[wmos] cmd: ping 10.0.2.2" \
    "[wmos] ping 10.0.2.2 ok" \
    "[wmos] cmd: exec /disk/bin/hello.elf" \
    "[wmos] user: Hello from WMOS Ring-3 ELF64 userland!" \
    "[wmos] user: Syscall ABI verified (CPL=3 -> Ring 0 -> CPL=3)." \
    "[wmos] user elf exit=0" \
    "[wmos] cmd: open iot"
do
    if ! grep -qF "${uefi_marker}" "dist/qemu-uefi.log"; then
        echo "ERROR: UEFI log missing interactive marker: ${uefi_marker}" >&2
        exit 1
    fi
done

echo "--- Mock IoT Hub log (dist/iot-mock.log) ---"
if [[ ! -f "dist/iot-mock.log" ]]; then
    echo "ERROR: dist/iot-mock.log missing" >&2
    exit 1
fi
cat dist/iot-mock.log
echo "--- end Mock IoT Hub log ---"

for iot_marker in \
    "HTTP GET /iot/lamp/on" \
    "MQTT home/lamp=on" \
    "UDP ON" \
    "MCU temp"
do
    if ! grep -qF "${iot_marker}" "dist/iot-mock.log"; then
        echo "ERROR: dist/iot-mock.log missing marker: ${iot_marker}" >&2
        exit 1
    fi
done

echo "=== Verifying FAT32 disk image contents with mtools ==="
mtype_out="$(mtype -i dist/wmos-persist-test.img@@2M ::/PERSIST.TXT 2>/dev/null || mtype -i dist/wmos-persist-test.img@@2M ::/persist.txt)"
echo "mtype ::/PERSIST.TXT => ${mtype_out}"
if ! echo "${mtype_out}" | grep -qF "hello-v4"; then
    echo "ERROR: mtype verification failed; 'hello-v4' not found in ::/PERSIST.TXT" >&2
    exit 1
fi

echo "=== [3/3] Verifying cold reboot persistence in QEMU UEFI (OVMF) Boot #2 ==="
qemu-system-x86_64 \
    -M q35 \
    -m 256M \
    -drive "if=pflash,format=raw,readonly=on,file=${OVMF_CODE}" \
    -cdrom dist/wmos.iso \
    -drive "file=dist/wmos-persist-test.img,format=raw" \
    -vga std \
    -display none \
    -serial file:dist/qemu-uefi-reboot.log \
    -monitor unix:/tmp/wmos-mon.sock,server,nowait \
    -no-reboot \
    -no-shutdown &
QEMU_PID=$!

reboot_ready=0
for _ in $(seq 1 60); do
    if [[ -f dist/qemu-uefi-reboot.log ]] && grep -qF "[wmos] ready" dist/qemu-uefi-reboot.log; then
        reboot_ready=1
        break
    fi
    if ! kill -0 "${QEMU_PID}" 2>/dev/null; then
        break
    fi
    sleep 0.25
done

if [[ ${reboot_ready} -eq 1 ]]; then
    sleep 0.3
    python3 - <<'PY' || true
import os
import socket
import time

sock_path = "/tmp/wmos-mon.sock"
reboot_log_path = "dist/qemu-uefi-reboot.log"

def recv_until_prompt(sock: socket.socket, timeout: float = 2.0) -> None:
    deadline = time.monotonic() + timeout
    buf = b""
    while time.monotonic() < deadline:
        try:
            chunk = sock.recv(4096)
            if not chunk:
                break
            buf += chunk
            if b"(qemu)" in buf:
                break
        except OSError:
            break

def send_mon(sock: socket.socket, cmd: bytes) -> None:
    sock.sendall(cmd + b"\n")
    recv_until_prompt(sock, timeout=2.0)

def map_key(ch: str) -> bytes:
    if "a" <= ch <= "z" or "0" <= ch <= "9":
        return ch.encode("ascii")
    if ch == " ":
        return b"spc"
    if ch == ".":
        return b"dot"
    if ch == "/":
        return b"slash"
    if ch == "-":
        return b"minus"
    raise ValueError(f"Unsupported key character: {ch!r}")

if os.path.exists(sock_path):
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
        s.settimeout(2.0)
        s.connect(sock_path)
        time.sleep(0.2)
        recv_until_prompt(s, timeout=1.0)

        for ch in "cat /disk/persist.txt":
            send_mon(s, b"sendkey " + map_key(ch))
            time.sleep(0.07)
        send_mon(s, b"sendkey ret")

        deadline = time.monotonic() + 4.0
        while time.monotonic() < deadline:
            if os.path.exists(reboot_log_path):
                with open(reboot_log_path, "r", encoding="utf-8", errors="replace") as lf:
                    if "[wmos] fs cat disk/persist.txt ok" in lf.read():
                        break
            time.sleep(0.1)
PY
fi

stop_qemu
rm -f dist/wmos-persist-test.img
trap - EXIT

assert_log "dist/qemu-uefi-reboot.log" "UEFI-Reboot"
if ! grep -qF "[wmos] fs cat disk/persist.txt ok" "dist/qemu-uefi-reboot.log"; then
    echo "ERROR: Cold reboot log missing '[wmos] fs cat disk/persist.txt ok'" >&2
    exit 1
fi

echo "=== Verification Summary ==="
echo "BIOS log: PASSED (all 20 required markers present, no PANIC/EXCEPTION)"
echo "UEFI Boot #1 log: PASSED (all 20 + FAT32 + e1000/DHCP/HTTP/MQTT/UDP/COM2 MCU + Ring-3 ELF64 markers present)"
echo "Mock IoT Hub log: PASSED (HTTP GET, MQTT PUBLISH, UDP ON, MCU temp verified)"
echo "mtools check: PASSED (::/PERSIST.TXT contains 'hello-v4')"
echo "UEFI Boot #2 (cold reboot): PASSED ('[wmos] fs cat disk/persist.txt ok', no PANIC/EXCEPTION)"
if [[ -f dist/boot-preview.png ]]; then
    echo "Preview PNG: dist/boot-preview.png generated"
else
    echo "Preview PNG: not generated (screendump optional)"
fi
