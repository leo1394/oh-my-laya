"""Install a Codex plugin; skills remain separately managed to avoid duplicates."""

import hashlib
import json
import os
import re
import shutil
import subprocess
import tempfile
import tomllib
from datetime import datetime, timezone
from pathlib import Path


NAME = "oh-my-laya"
MARKER = ".oh-my-laya.manifest.json"


def _files(root):
    result = {}
    for path in root.rglob("*"):
        if path.is_symlink():
            raise RuntimeError(f"Refusing symlink in plugin: {path}")
        if path.is_file() and path.name != MARKER:
            result[path.relative_to(root).as_posix()] = hashlib.sha256(path.read_bytes()).hexdigest()
    return result


def _safe_path(path):
    for item in (path, *path.parents):
        if item == Path.home():
            break
        if item.is_symlink():
            raise RuntimeError(f"Refusing symlinked plugin installation path: {item}")


def install_codex_plugin(executable, server, model_dir, dry_run, run):
    destination = Path.home() / "plugins" / NAME
    catalog = Path.home() / ".agents" / "plugins" / "marketplace.json"
    _safe_path(destination)
    _safe_path(catalog)
    previous = catalog.read_bytes() if catalog.exists() else None
    marketplace = json.loads(previous) if previous is not None else {
        "name": "personal", "interface": {"displayName": "Personal"}, "plugins": []
    }
    if not isinstance(marketplace, dict) or not re.fullmatch(
        r"[A-Za-z0-9_-]+", str(marketplace.get("name", ""))
    ) or not isinstance(marketplace.get("plugins"), list):
        raise RuntimeError(f"Invalid personal marketplace: {catalog}")
    entry = {
        "name": NAME, "source": {"source": "local", "path": f"./plugins/{NAME}"},
        "policy": {"installation": "AVAILABLE", "authentication": "ON_INSTALL"},
        "category": "Productivity",
    }
    matches = [item for item in marketplace["plugins"]
               if isinstance(item, dict) and item.get("name") == NAME]
    if len(matches) > 1 or (matches and matches[0] != entry):
        raise RuntimeError(f"Conflicting {NAME} marketplace entry; preserve it first: {catalog}")
    if not matches:
        marketplace["plugins"].append(entry)
    if destination.exists():
        marker = destination / MARKER
        if not marker.is_file() or json.loads(marker.read_text()) != _files(destination):
            raise RuntimeError(f"Unmanaged or locally modified plugin; preserve it first: {destination}")
    selector = f"{NAME}@{marketplace['name']}"
    print(f"+ install Codex plugin {selector} from {destination}")
    if dry_run:
        run([executable, "plugin", "add", selector], dry_run=True)
        print("+ remove matching legacy MCP only after plugin installation succeeds")
        return
    subprocess.run([executable, "plugin", "add", "--help"], check=True, capture_output=True)
    destination.parent.mkdir(parents=True, exist_ok=True)
    catalog.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".laya-plugin-", dir=destination.parent) as temporary:
        staging = Path(temporary) / NAME
        backup = Path(temporary) / "previous"
        shutil.copytree(Path(__file__).parent / "plugin" / NAME, staging)
        manifest_path = staging / ".codex-plugin" / "plugin.json"
        manifest = json.loads(manifest_path.read_text())
        version = manifest["version"].split("+", 1)[0]
        manifest["version"] = version + "+codex." + datetime.now(timezone.utc).strftime("%Y%m%d%H%M%S%f")
        manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")
        (staging / ".mcp.json").write_text(json.dumps({"mcpServers": {NAME: {
            "command": str(server), "args": [], "env": {"LAYA_MODEL_DIR": str(model_dir)}
        }}}, indent=2) + "\n")
        (staging / MARKER).write_text(json.dumps(_files(staging), indent=2) + "\n")
        if destination.exists():
            destination.rename(backup)
        try:
            staging.rename(destination)
            catalog.write_text(json.dumps(marketplace, indent=2) + "\n")
            run([executable, "plugin", "add", selector])
        except BaseException:
            if destination.exists():
                shutil.rmtree(destination)
            if backup.exists():
                backup.rename(destination)
            if previous is None:
                catalog.unlink(missing_ok=True)
            else:
                catalog.write_bytes(previous)
            raise
    # mcp get also resolves plugin servers: only migrate an explicit config entry.
    config = Path(os.environ.get("CODEX_HOME", str(Path.home() / ".codex"))) / "config.toml"
    transport = tomllib.loads(config.read_text()).get("mcp_servers", {}).get(NAME) if config.exists() else None
    if transport is not None:
        owned = (transport.get("command") == str(server)
                 and transport.get("env", {}).get("LAYA_MODEL_DIR") == str(model_dir))
        if owned:
            run([executable, "mcp", "remove", NAME])
        else:
            print("! Existing standalone oh-my-laya MCP was preserved: configuration differs. Remove it manually if redundant.")
    print("+ Codex plugin installed. Open Plugins → Personal → Oh My Laya, then start a new task.")
