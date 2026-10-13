#!/usr/bin/env bash
set -euo pipefail
fixture=$(mktemp)
device=''
original_mode=''
cleanup() {
    if [[ -n "$device" ]]; then
        if [[ -n "$original_mode" ]]; then sudo chmod "$original_mode" "$device"; fi
        sudo losetup -d "$device"
    fi
    rm -f "$fixture"
}
trap cleanup EXIT
python3 - "$fixture" <<'PY'
import sys
with open(sys.argv[1], 'wb') as image:
    # Mix the full 512-byte sector number into each eight-byte group.
    image.write(bytes((offset & 255) ^ (((offset >> 9) >> (8 * (offset % 8))) & 255)
                      for offset in range(8 * 1024 * 1024)))
PY
for sector in 512 4096; do
    device=$(sudo losetup --find --show --read-only --sector-size "$sector" "$fixture")
    original_mode=$(stat -c %a "$device")
    sudo chmod a+r "$device"
    VIRTDISK_TEST_DEVICE="$device" VIRTDISK_TEST_SECTOR_SIZE="$sector" cargo test --all-features --locked --test physical_disk -- --ignored
    sudo chmod "$original_mode" "$device"
    sudo losetup -d "$device"
    device=''
done
