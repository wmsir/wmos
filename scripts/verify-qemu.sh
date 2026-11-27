#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${REPO_ROOT}"

if [[ ! -f "dist/wmos.iso" ]]; then
    echo "ERROR: dist/wmos.iso not found. Run ./scripts/build-images.sh first." >&2
    exit 1
fi

rm -f dist/qemu-bios.log dist/qemu-uefi.log dist/boot-preview.ppm dist/boot-preview.png /tmp/wmos-mon.sock

echo "=== [1/2] Verifying dist/wmos.iso in QEMU Legacy BIOS mode ==="
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

echo "=== [2/2] Verifying dist/wmos.iso in QEMU UEFI (OVMF) mode ==="
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

QEMU_PID=""
cleanup_uefi() {
    if [[ -n "${QEMU_PID}" ]] && kill -0 "${QEMU_PID}" 2>/dev/null; then
        kill "${QEMU_PID}" 2>/dev/null || true
        wait "${QEMU_PID}" 2>/dev/null || true
    fi
    rm -f /tmp/wmos-mon.sock
}
trap cleanup_uefi EXIT

qemu-system-x86_64 \
    -M q35 \
    -m 256M \
    -drive "if=pflash,format=raw,readonly=on,file=${OVMF_CODE}" \
    -cdrom dist/wmos.iso \
    -vga std \
    -display none \
    -serial file:dist/qemu-uefi.log \
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
    raise ValueError(f"Unsupported key character: {ch!r}")

def send_command(sock: socket.socket, text: str) -> None:
    for ch in text:
        send_mon(sock, b"sendkey " + map_key(ch))
        time.sleep(0.07)
    send_mon(sock, b"sendkey ret")
    time.sleep(0.07)

if os.path.exists(sock_path):
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
        s.settimeout(2.0)
        s.connect(sock_path)
        time.sleep(0.2)
        recv_until_prompt(s, timeout=1.0)

        send_mon(s, b"mouse_move 40 25")
        time.sleep(0.08)

        for cmd in (
            "ls",
            "write demo.txt hello",
            "cat demo.txt",
            "date",
            "open paint",
        ):
            send_command(s, cmd)

        deadline = time.monotonic() + 4.0
        while time.monotonic() < deadline:
            if os.path.exists(uefi_log_path):
                with open(uefi_log_path, "r", encoding="utf-8", errors="replace") as lf:
                    if "[wmos] cmd: open paint" in lf.read():
                        break
            time.sleep(0.1)

        time.sleep(0.25)
        send_mon(s, b"screendump dist/boot-preview.ppm")
PY
fi

cleanup_uefi
QEMU_PID=""
trap - EXIT

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
        "[wmos] pmm ok"
        "[wmos] vmm self-test ok"
        "[wmos] heap ok"
        "[wmos] heap self-test ok"
        "[wmos] rtc ok"
        "[wmos] fs ok"
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
    "[wmos] mouse active" \
    "[wmos] cmd: ls" \
    "[wmos] cmd: write demo.txt hello" \
    "[wmos] cmd: cat demo.txt" \
    "[wmos] fs cat demo.txt ok" \
    "[wmos] cmd: date" \
    "[wmos] cmd: open paint"
do
    if ! grep -qF "${uefi_marker}" "dist/qemu-uefi.log"; then
        echo "ERROR: UEFI log missing interactive marker: ${uefi_marker}" >&2
        exit 1
    fi
done

echo "=== Verification Summary ==="
echo "BIOS log: PASSED (all required markers present, no PANIC/EXCEPTION)"
echo "UEFI log: PASSED (all required + interactive markers present, no PANIC/EXCEPTION)"
if [[ -f dist/boot-preview.png ]]; then
    echo "Preview PNG: dist/boot-preview.png generated"
else
    echo "Preview PNG: not generated (screendump optional)"
fi
