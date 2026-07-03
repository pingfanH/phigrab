#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
API_URL="${API_URL:-https://phira.pingfanh.top}"
LANG_HEADER="${LANG_HEADER:-zh-CN}"

default_version() {
  sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT_DIR/Cargo.toml" | head -n1
}

default_flavor() {
  local flavor_file="${ROOT_DIR}/flavor"
  if [[ -f "$flavor_file" ]]; then
    tr -d '\r\n' < "$flavor_file"
  else
    printf 'none'
  fi
}

VERSION="${1:-${VERSION:-$(default_version)}}"
FLAVOR="${2:-${FLAVOR:-$(default_flavor)}}"

if [[ -z "$VERSION" ]]; then
  echo "failed to detect version, set VERSION=... or pass it as the first argument" >&2
  exit 1
fi

echo "GET ${API_URL}/check-update"
echo "version=${VERSION}"
echo "flavor=${FLAVOR}"

tmp_body="$(mktemp)"
trap 'rm -f "$tmp_body"' EXIT

http_code="$(curl -sS -G \
  -H "Accept-Language: ${LANG_HEADER}" \
  --data-urlencode "version=${VERSION}" \
  --data-urlencode "flavor=${FLAVOR}" \
  -o "$tmp_body" \
  -w "%{http_code}" \
  "${API_URL}/check-update")"

echo "status=${http_code}"

if command -v jq >/dev/null 2>&1; then
  jq . "$tmp_body"
else
  cat "$tmp_body"
fi

if [[ "$http_code" -lt 200 || "$http_code" -ge 300 ]]; then
  exit 1
fi
