#!/bin/sh
set -eu

if [ "$(uname -s)" != Darwin ] || [ "$(uname -m)" != arm64 ]; then
  echo "error: Oh My Laya requires an Apple Silicon Mac" >&2
  exit 1
fi

for dependency in bash; do
  if ! command -v "$dependency" >/dev/null 2>&1; then
    echo "error: $dependency is required" >&2
    exit 1
  fi
done

if command -v git >/dev/null 2>&1; then
  LAYA_DOWNLOADER=git
elif command -v curl >/dev/null 2>&1; then
  LAYA_DOWNLOADER=curl
elif command -v wget >/dev/null 2>&1; then
  LAYA_DOWNLOADER=wget
else
  echo "error: git, curl or wget is required" >&2
  exit 1
fi

if [ "$LAYA_DOWNLOADER" != git ] && ! command -v tar >/dev/null 2>&1; then
  echo "error: tar is required for archive downloads" >&2
  exit 1
fi

LAYA_BOOTSTRAP_DIR=$(mktemp -d "${TMPDIR:-/tmp}/oh-my-laya.XXXXXXXX")
trap 'rm -rf "$LAYA_BOOTSTRAP_DIR"' 0
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

echo "Downloading Oh My Laya..."
if [ "$LAYA_DOWNLOADER" = git ]; then
  git clone --depth 1 --single-branch --branch master \
    https://github.com/leo1394/oh-my-laya.git "$LAYA_BOOTSTRAP_DIR/source"
else
  LAYA_SOURCE_URL=https://codeload.github.com/leo1394/oh-my-laya/tar.gz/refs/heads/master
  if [ "$LAYA_DOWNLOADER" = curl ]; then
    curl --fail --show-error --silent --location \
      "$LAYA_SOURCE_URL" --output "$LAYA_BOOTSTRAP_DIR/source.tar.gz"
  else
    wget -O "$LAYA_BOOTSTRAP_DIR/source.tar.gz" "$LAYA_SOURCE_URL"
  fi
  mkdir "$LAYA_BOOTSTRAP_DIR/source"
  tar -xzf "$LAYA_BOOTSTRAP_DIR/source.tar.gz" \
    -C "$LAYA_BOOTSTRAP_DIR/source" --strip-components=1
fi

if [ ! -f "$LAYA_BOOTSTRAP_DIR/source/install.sh" ]; then
  echo "error: Downloaded source does not contain install.sh" >&2
  exit 1
fi

bash "$LAYA_BOOTSTRAP_DIR/source/install.sh" --targets all "$@"
