#!/usr/bin/env python3
"""Synchronize release metadata from VERSION.txt; default is a read-only check."""
import argparse
from pathlib import Path
import re
from datetime import date


ROOT = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--write', action='store_true')
    parser.add_argument('--tag')
    args = parser.parse_args()
    version = (ROOT / 'VERSION.txt').read_text().strip()
    release_date = (ROOT / 'RELEASE_DATE.txt').read_text().strip()
    try:
        if date.fromisoformat(release_date).isoformat() != release_date:
            raise ValueError()
    except ValueError:
        parser.error('RELEASE_DATE.txt must contain the release date as YYYY-MM-DD')
    if not re.fullmatch(r'(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)', version):
        parser.error('VERSION.txt must contain one stable semantic version')
    if args.tag and args.tag != 'v' + version:
        parser.error('tag must match VERSION.txt')
    targets = {
        'pyproject.toml': (r'(\[project\]\nname = "oh-my-laya"\nversion = ")[^"]+', 1),
        'crates/laya/Cargo.toml': (r'(\nversion = ")[^"]+', 1),
        'Cargo.lock': (r'(name = "laya"\nversion = ")[^"]+', 1),
        'src/laya_tell_me/runtime_install.py': (r'(WORKBENCH_VERSION = ")[^"]+', 1),
        'web/package.json': (r'("version": ")[^"]+', 1),
        'web/package-lock.json': (r'("version": ")[^"]+', 2),
        'src/laya_tell_me/plugin/oh-my-laya/.codex-plugin/plugin.json': (r'("version": ")[^"]+', 1),
    }
    mismatches = []
    for name, (pattern, count) in targets.items():
        path = ROOT / name
        text = path.read_text()
        updated, found = re.subn(pattern, lambda match: match[1] + version, text, count=count)
        if found != count:
            parser.error('unrecognized version metadata: ' + name)
        if updated != text:
            mismatches.append(name)
            if args.write:
                path.write_text(updated)
    if mismatches and not args.write:
        parser.error('version differs from VERSION.txt: ' + ', '.join(mismatches))
    print(version)


if __name__ == '__main__':
    main()
