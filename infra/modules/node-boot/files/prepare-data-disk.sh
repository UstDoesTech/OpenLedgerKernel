#!/usr/bin/env bash
# Finds the data disk by its exact size, makes a file system on it the first time, and mounts
# it. Disk names differ between clouds (NVMe on AWS, SCSI on Azure, by-id on Google), so the
# size is what identifies it. It refuses to guess: exactly one unpartitioned disk of that size
# must exist, and a disk that already holds someone else's file system is never touched.
set -euo pipefail

size_bytes="$1"
target="$2"

mkdir -p "$target"
if mountpoint -q "$target"; then
  exit 0
fi

candidates=()
while read -r path size type; do
  if [ "$type" = disk ] && [ "$size" = "$size_bytes" ]; then
    # A disk with partitions is a boot disk or in use. Skip it.
    if [ "$(lsblk -nro NAME "$path" | wc -l)" = 1 ]; then
      candidates+=("$path")
    fi
  fi
done < <(lsblk -bdnpo PATH,SIZE,TYPE)

if [ "${#candidates[@]}" -ne 1 ]; then
  echo "expected one data disk of $size_bytes bytes, found ${#candidates[@]}" >&2
  exit 1
fi
disk="${candidates[0]}"

fs_type=$(blkid -o value -s TYPE "$disk" || true)
if [ -z "$fs_type" ]; then
  mkfs.ext4 -q -L ledger-data "$disk"
elif [ "$(blkid -o value -s LABEL "$disk" || true)" != ledger-data ]; then
  echo "$disk already holds a file system that is not ours" >&2
  exit 1
fi

mount -o noatime LABEL=ledger-data "$target"
# The container's postgres user is 999.
chown 999:999 "$target"
