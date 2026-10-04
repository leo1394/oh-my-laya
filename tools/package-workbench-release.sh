#!/bin/sh
set -eu

project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
release_dir=${1:-"$project_dir/dist/workbench"}
target_dir=${CARGO_TARGET_DIR:-"$project_dir/target"}
case "$target_dir" in
  /*) ;;
  *) target_dir="$project_dir/$target_dir" ;;
esac

if [ "$(uname -s)" != Darwin ] || [ "$(uname -m)" != arm64 ]; then
  echo "error: workbench release artifacts must be built on macOS arm64" >&2
  exit 1
fi

cd "$project_dir/web"
npm ci
npm run build
test -f dist/index.html

cd "$project_dir"
cargo build --locked --release --package laya --target-dir "$target_dir"
test -x "$target_dir/release/laya"

mkdir -p "$release_dir"
cp "$target_dir/release/laya" "$release_dir/laya-macos-arm64"
chmod 755 "$release_dir/laya-macos-arm64"
(cd "$release_dir" && shasum -a 256 laya-macos-arm64 > laya-macos-arm64.sha256)

echo "Release artifacts: $release_dir/laya-macos-arm64{,.sha256}"
