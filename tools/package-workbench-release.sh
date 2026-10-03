#!/bin/sh
set -eu

project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
release_dir=${1:-"$project_dir/dist/workbench"}

if [ "$(uname -s)" != Darwin ] || [ "$(uname -m)" != arm64 ]; then
  echo "error: workbench release artifacts must be built on macOS arm64" >&2
  exit 1
fi

cd "$project_dir/web"
npm ci
npm run build
test -f dist/index.html

cd "$project_dir"
cargo build --locked --release --package laya
test -x target/release/laya

mkdir -p "$release_dir"
cp target/release/laya "$release_dir/laya-macos-arm64"
chmod 755 "$release_dir/laya-macos-arm64"
(cd "$release_dir" && shasum -a 256 laya-macos-arm64 > laya-macos-arm64.sha256)

echo "Release artifacts: $release_dir/laya-macos-arm64{,.sha256}"
