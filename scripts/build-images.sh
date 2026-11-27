#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${REPO_ROOT}"

KERNEL_BIN="kernel/target/x86_64-unknown-none/release/wmos-kernel"

if [[ ! -f "${KERNEL_BIN}" ]]; then
    echo "ERROR: Kernel binary not found at ${KERNEL_BIN}" >&2
    echo "Run: (cd kernel && cargo build --release)" >&2
    exit 1
fi

if [[ ! -x "limine/limine" ]]; then
    if [[ ! -d "limine/.git" ]]; then
        rm -rf limine
        git clone https://github.com/limine-bootloader/limine.git --branch=v10.x-binary --depth=1 limine
    fi
    make -C limine
fi

mkdir -p dist

# Assemble and link Ring-3 userland ELF64 program (dist/hello.elf)
if [[ ! -f "user/hello.S" ]]; then
    echo "ERROR: user/hello.S not found" >&2
    exit 1
fi
gcc -nostdlib -static -no-pie -Wl,-Ttext=0x400000 -Wl,--build-id=none user/hello.S -o dist/hello.elf

# Build hybrid BIOS + UEFI ISO (dist/wmos.iso)
rm -rf iso_root
mkdir -p iso_root/boot/limine iso_root/EFI/BOOT iso_root/bin

cp "${KERNEL_BIN}" iso_root/boot/wmos-kernel
cp boot/limine.conf iso_root/boot/limine/limine.conf
cp limine/limine-bios.sys limine/limine-bios-cd.bin limine/limine-uefi-cd.bin iso_root/boot/limine/
cp limine/BOOTX64.EFI limine/BOOTIA32.EFI iso_root/EFI/BOOT/
cp dist/hello.elf iso_root/bin/hello.elf

rm -f dist/wmos.iso
xorriso -as mkisofs \
    -b boot/limine/limine-bios-cd.bin \
    -no-emul-boot -boot-load-size 4 -boot-info-table \
    --efi-boot boot/limine/limine-uefi-cd.bin \
    -efi-boot-part --efi-boot-image --protective-msdos-label \
    iso_root -o dist/wmos.iso

./limine/limine bios-install dist/wmos.iso

# Build bootable 64 MiB GPT FAT32 raw disk image (dist/wmos.img)
rm -f dist/wmos.img
dd if=/dev/zero bs=1M count=0 seek=64 of=dist/wmos.img
sgdisk dist/wmos.img \
    -n 1:2048:4095 -t 1:ef02 \
    -n 2:4096:+60M -t 2:ef00
./limine/limine bios-install dist/wmos.img 1

mformat -i dist/wmos.img@@2M -T 122880 -F ::
mmd -i dist/wmos.img@@2M ::/EFI ::/EFI/BOOT ::/boot ::/boot/limine ::/bin
mcopy -i dist/wmos.img@@2M "${KERNEL_BIN}" ::/boot/wmos-kernel
mcopy -i dist/wmos.img@@2M boot/limine.conf ::/boot/limine/limine.conf
mcopy -i dist/wmos.img@@2M limine/limine-bios.sys ::/boot/limine/
mcopy -i dist/wmos.img@@2M limine/BOOTX64.EFI limine/BOOTIA32.EFI ::/EFI/BOOT/
mcopy -i dist/wmos.img@@2M dist/hello.elf ::/bin/hello.elf

# Convert raw disk image to VM formats
qemu-img convert -f raw -O qcow2 dist/wmos.img dist/wmos.qcow2
qemu-img convert -f raw -O vmdk dist/wmos.img dist/wmos.vmdk
qemu-img convert -f raw -O vpc -o subformat=fixed dist/wmos.img dist/wmos.vhd

# Compute checksums
(
    cd dist
    sha256sum wmos.iso wmos.img wmos.qcow2 wmos.vmdk wmos.vhd > SHA256SUMS.txt
)

ls -lh dist/
