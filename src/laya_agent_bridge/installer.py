import argparse
import json
import os
import platform
import re
import shutil
import subprocess
import sys
import venv
from dataclasses import dataclass
from pathlib import Path

from .models import MODELS


SERVER_NAME = "oh-my-laya"
MANAGED_START = "# >>> oh-my-laya >>>"
MANAGED_END = "# <<< oh-my-laya <<<"


@dataclass(frozen=True)
class Client:
    key: str
    label: str
    executable: str | None


def detect_clients(which=shutil.which) -> list[Client]:
    return [
        Client("codex", "Codex", which("codex")),
        Client("claude", "Claude Code", which("claude")),
        Client("dsh", "DeepSeek Harness", which("dsh")),
        Client("pi", "pi-agent", which("pi")),
    ]


def parse_targets(value: str, detected: set[str]) -> list[str]:
    requested = [item.strip().lower() for item in value.split(",") if item.strip()]
    if not requested:
        raise ValueError("no targets selected")
    if requested in (["all"], ["both"]):
        if not detected:
            raise ValueError("none of the supported clients were detected")
        return sorted(detected)

    aliases = {"deepseek": "dsh", "deepseek-harness": "dsh", "pi-agent": "pi"}
    normalized = [aliases.get(item, item) for item in requested]
    unknown = sorted(set(normalized) - {"codex", "claude", "dsh", "pi"})
    if unknown:
        raise ValueError(f"unknown targets: {', '.join(unknown)}")
    missing = sorted(set(normalized) - detected)
    if missing:
        raise ValueError(f"requested clients are not installed: {', '.join(missing)}")
    return list(dict.fromkeys(normalized))


def prompt_targets(clients: list[Client]) -> list[str]:
    detected = [client for client in clients if client.executable]
    if not detected:
        raise RuntimeError("No supported agent CLI was detected")

    print("Detected agent clients:")
    for index, client in enumerate(detected, start=1):
        print(f"  {index}) {client.label} ({client.executable})")
    print("  a) All detected clients")
    print("Select one, multiple (for example 1,3), or 'a' for all.")

    raw = input("Targets: ").strip().lower()
    if raw in {"a", "all", "both"}:
        return [client.key for client in detected]

    selected = []
    for item in raw.split(","):
        try:
            client = detected[int(item.strip()) - 1]
        except (ValueError, IndexError):
            raise ValueError(f"invalid selection: {raw}") from None
        if client.key not in selected:
            selected.append(client.key)
    if not selected:
        raise ValueError("no targets selected")
    return selected


def replace_managed_block(existing: str, block: str) -> str:
    pattern = re.compile(
        rf"(?:^|\n){re.escape(MANAGED_START)}.*?{re.escape(MANAGED_END)}(?:\n|$)",
        re.DOTALL,
    )
    cleaned = pattern.sub("\n", existing).rstrip()
    return f"{cleaned}\n\n{block.strip()}\n" if cleaned else f"{block.strip()}\n"


def dsh_block(command: Path, model_dir: Path, install_dir: Path) -> str:
    return f"""{MANAGED_START}
- insert:
    - id: mcp-oh-my-laya
      name: '@deepseek-ai/dsh-mcp-client'
      config:
        serverName: oh-my-laya
        transport: stdio
        command: '{command}'
        args: []
        env:
          LAYA_MODEL_DIR: '{model_dir}'
        cwd: '{install_dir}'
        toolCallTimeoutMs: 120000
        failOnStartupError: false
{MANAGED_END}"""


def run(command: list[str], *, check=True, cwd=None, dry_run=False):
    print("+", " ".join(str(part) for part in command))
    if dry_run:
        return subprocess.CompletedProcess(command, 0, "", "")
    return subprocess.run(command, check=check, cwd=cwd, text=True)


def register_codex(executable: str, server: Path, model_dir: Path, dry_run: bool):
    run([executable, "mcp", "remove", SERVER_NAME], check=False, dry_run=dry_run)
    run(
        [
            executable,
            "mcp",
            "add",
            SERVER_NAME,
            "--env",
            f"LAYA_MODEL_DIR={model_dir}",
            "--",
            str(server),
        ],
        dry_run=dry_run,
    )


def register_claude(executable: str, server: Path, model_dir: Path, dry_run: bool):
    run(
        [executable, "mcp", "remove", SERVER_NAME, "--scope", "user"],
        check=False,
        dry_run=dry_run,
    )
    run(
        [
            executable,
            "mcp",
            "add",
            SERVER_NAME,
            "--scope",
            "user",
            "--env",
            f"LAYA_MODEL_DIR={model_dir}",
            "--",
            str(server),
        ],
        dry_run=dry_run,
    )


def register_dsh(server: Path, model_dir: Path, install_dir: Path, dry_run: bool):
    dsh_home = Path(os.environ.get("DSH_HOME", "~/.dsh")).expanduser()
    patch_file = dsh_home / "cordis.patch.yml"
    block = dsh_block(server, model_dir, install_dir)
    existing = patch_file.read_text() if patch_file.exists() else ""
    updated = replace_managed_block(existing, block)
    print(f"+ update {patch_file}")
    if not dry_run:
        patch_file.parent.mkdir(parents=True, exist_ok=True)
        patch_file.write_text(updated)


def register_pi(
    executable: str,
    source_root: Path,
    install_dir: Path,
    server: Path,
    model_dir: Path,
    dry_run: bool,
):
    source = source_root / "integrations" / "pi"
    destination = install_dir / "pi-package"
    print(f"+ copy {source} -> {destination}")
    if not dry_run:
        if destination.exists():
            shutil.rmtree(destination)
        shutil.copytree(source, destination)
        config = json.dumps(
            {"command": str(server), "args": [], "modelDir": str(model_dir)},
            indent=2,
        ) + "\n"
        (destination / "laya.config.json").write_text(config)

    run([executable, "remove", str(destination)], check=False, dry_run=dry_run)
    run([executable, "install", str(destination)], dry_run=dry_run)


def ensure_platform():
    if platform.system() != "Darwin" or platform.machine() != "arm64":
        raise RuntimeError("Oh My Laya requires an Apple Silicon Mac")
    if sys.version_info < (3, 11):
        raise RuntimeError("Python 3.11 or newer is required")


def install_runtime(source_root: Path, install_dir: Path, model: str, dry_run: bool):
    venv_dir = install_dir / "venv"
    model_dir = install_dir / "models" / model
    python = venv_dir / "bin" / "python"
    server = venv_dir / "bin" / "oh-my-laya-mcp"

    print(f"+ create/update virtual environment at {venv_dir}")
    if not dry_run:
        install_dir.mkdir(parents=True, exist_ok=True)
        if not python.exists():
            venv.EnvBuilder(with_pip=True).create(venv_dir)
        run([str(python), "-m", "pip", "install", "--upgrade", str(source_root)])
        run(
            [
                str(python),
                "-m",
                "laya_agent_bridge.download",
                "--model",
                model,
                "--destination",
                str(model_dir),
            ]
        )
    return server, model_dir


def build_parser():
    parser = argparse.ArgumentParser(
        description="Install Oh My Laya and register it with local agent clients"
    )
    parser.add_argument("--source-root", type=Path)
    parser.add_argument(
        "--targets",
        help="comma-separated: codex,claude,dsh,pi; use all/both for every detected client",
    )
    parser.add_argument("--model", choices=sorted(MODELS), default="multilingual")
    parser.add_argument(
        "--install-dir",
        type=Path,
        default=Path("~/.local/share/oh-my-laya").expanduser(),
    )
    parser.add_argument("--dry-run", action="store_true")
    return parser


def main(argv=None):
    args = build_parser().parse_args(argv)
    ensure_platform()

    source_root = (args.source_root or Path.cwd()).expanduser().resolve()
    if not (source_root / "pyproject.toml").is_file():
        raise RuntimeError(f"Not a project root: {source_root}")

    clients = detect_clients()
    detected = {client.key for client in clients if client.executable}
    targets = (
        parse_targets(args.targets, detected)
        if args.targets
        else prompt_targets(clients)
    )

    install_dir = args.install_dir.expanduser().resolve()
    server, model_dir = install_runtime(
        source_root, install_dir, args.model, args.dry_run
    )
    by_key = {client.key: client for client in clients}

    for target in targets:
        executable = by_key[target].executable
        if target == "codex":
            register_codex(executable, server, model_dir, args.dry_run)
        elif target == "claude":
            register_claude(executable, server, model_dir, args.dry_run)
        elif target == "dsh":
            register_dsh(server, model_dir, install_dir, args.dry_run)
        elif target == "pi":
            register_pi(
                executable,
                source_root,
                install_dir,
                server,
                model_dir,
                args.dry_run,
            )

    print()
    print(f"Installed model: {args.model}")
    print(f"Registered clients: {', '.join(targets)}")
    print("Restart active agent sessions so they refresh their tool lists.")


if __name__ == "__main__":
    main()
