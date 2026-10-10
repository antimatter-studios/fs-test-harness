#!/bin/bash
# run-timed.sh MODE LABEL [MATRIX-FILTER...]
# One NTFS matrix run on the QEMU guest from a fresh boot. MODE is scp or
# virtiofs; everything else is identical between modes.
set -uo pipefail
MODE="$1"; LABEL="$2"; shift 2
case "$MODE" in scp|virtiofs) ;; *) echo "MODE must be scp or virtiofs" >&2; exit 2 ;; esac
V=${HOME}/wt/qemu-macos-validation
H="$V/fs-windows-test-harness"; N="$V/rust-fs-ntfs"; E="$V/evidence"
SHARE="$V/share-images"; VIRTIOFSD="$V/virtiofsd/target/release/virtiofsd"
VM="python3 $H/scripts/local-vm.py"

$VM down || exit 1
if [ "$MODE" = virtiofs ]; then
  $VM up --share "$SHARE" --virtiofsd "$VIRTIOFSD" || exit 1
else
  $VM up || exit 1
fi
$VM wait --timeout 900 || exit 1

{
  echo "VM_HOST=fswth-local"
  echo "SSH_KEY="
  echo "VM_WORKDIR=C:/fswth/rust-fs-ntfs-timed"
  echo "HOST_IMAGE_DIR=$SHARE"
  if [ "$MODE" = virtiofs ]; then
    echo "VM_SHARE_HOST_DIR=$SHARE"
    echo "VM_SHARE_GUEST_DIR=Z:/"
  fi
} > "$N/.test-env"
cp "$N/.test-env" "$E/timed-$LABEL-test-env.txt"
if [ -d ${CONSUMER}/test-diagnostics/matrix ]; then
  rm -r ${CONSUMER}/test-diagnostics/matrix
fi
$VM ssh -- "$(cat "$E/guest-info.ps1")" > "$E/timed-$LABEL-guest-before.json"
pmset -g batt | head -1 > "$E/timed-$LABEL-power.txt"

cd "$N" || exit 1
"$E/run-logged.sh" "timed-$LABEL" -- $VM exec -- \
  bash ../rust-fs-core/scripts/tier.sh --refuse-skips --refuse-ignored \
  matrix -- bash scripts/run-matrix.sh "$@"
status=$?
cp tmp/logs/matrix.log "$E/logs/timed-$LABEL-matrix.log"
$VM exec -- bash scripts/matrix-fetch-diag.sh > "$E/logs/timed-$LABEL-diag.log" 2>&1
$VM exec -- bash scripts/vm-clean-images.sh >> "$E/logs/timed-$LABEL-diag.log" 2>&1
mkdir -p "$E/diag-timed-$LABEL"
cp -R test-diagnostics/matrix "$E/diag-timed-$LABEL/"
$VM ssh -- "$(cat "$E/guest-info.ps1")" > "$E/timed-$LABEL-guest-after.json"
python3 "$E/analyze.py" "$N/test-matrix.json" "$E/diag-timed-$LABEL/matrix" "$E/results-timed-$LABEL.json" > "$E/timed-$LABEL-summary.json"
$VM down
echo "timed-$LABEL: matrix exit $status"
