#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
EXPECTED_IDENTITY="cc-partner Internal Code Signing"
EXPECTED_BUNDLE_ID="com.cc-partner.app"

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "[build-macos-signed] 仅支持 macOS" >&2
  exit 1
fi
if [[ "${CC_PARTNER_INTERNAL_SIGNING_IDENTITY:-}" != "$EXPECTED_IDENTITY" ]]; then
  echo "[build-macos-signed] CC_PARTNER_INTERNAL_SIGNING_IDENTITY 必须为 $EXPECTED_IDENTITY" >&2
  exit 1
fi
if [[ -z "${CC_PARTNER_INTERNAL_CERT_SHA256:-}" ]]; then
  echo "[build-macos-signed] 缺少 CC_PARTNER_INTERNAL_CERT_SHA256" >&2
  exit 1
fi
if ! security find-identity -v -p codesigning | grep -Fq "\"$EXPECTED_IDENTITY\""; then
  echo "[build-macos-signed] Keychain 中找不到固定签名 identity: $EXPECTED_IDENTITY" >&2
  exit 1
fi

cd "$REPO_ROOT"
node scripts/prepare-tauri-sidecar.mjs
web/node_modules/.bin/tauri build --config src-tauri/tauri.internal.conf.json

APP_PATH="$REPO_ROOT/src-tauri/target/release/bundle/macos/cc-partner.app"
if [[ ! -d "$APP_PATH" ]]; then
  echo "[build-macos-signed] 未找到构建产物: $APP_PATH" >&2
  exit 1
fi
node scripts/check-macos-signing-contract.mjs "$APP_PATH" "$EXPECTED_BUNDLE_ID"
echo "[build-macos-signed] macOS 固定签名构建完成: $APP_PATH"
