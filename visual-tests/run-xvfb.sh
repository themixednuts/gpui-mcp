#!/usr/bin/env bash
set -euo pipefail

mkdir -p visual-tests/artifacts
openbox --sm-disable >visual-tests/artifacts/openbox.log 2>&1 &
openbox_pid=$!

cleanup() {
  kill "$openbox_pid" 2>/dev/null || true
  wait "$openbox_pid" 2>/dev/null || true
}
trap cleanup EXIT

npm run test:visual
