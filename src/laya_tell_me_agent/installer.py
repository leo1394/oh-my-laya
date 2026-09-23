import argparse
import hashlib
import json
import os
import platform
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import venv
from dataclasses import dataclass
from pathlib import Path
from urllib import request
import zipfile

from .models import MODELS
from .advisor import POLICIES, preferences
from .goal_workflow import client_home, install_goal_workflow, select_goal_workflow


SERVER_NAME = "oh-my-laya"
MANAGED_START = "# >>> oh-my-laya >>>"
MANAGED_END = "# <<< oh-my-laya <<<"
MACOS_CODEX_EXECUTABLES = (
    Path("/Applications/Codex.app/Contents/Resources/codex"),
    Path("/Applications/ChatGPT.app/Contents/Resources/codex"),
    Path("~/Applications/Codex.app/Contents/Resources/codex"),
    Path("~/Applications/ChatGPT.app/Contents/Resources/codex"),
)


@dataclass(frozen=True)
class Client:
    key: str
    label: str
    executable: str | None


def resolve_executable(
    name: str,
    *,
    which=shutil.which,
    fallback_paths: tuple[Path, ...] | None = None,
) -> str | None:
    executable = which(name)
    if executable:
        return executable

    if fallback_paths is None:
        fallback_paths = (
            MACOS_CODEX_EXECUTABLES
            if name == "codex" and platform.system() == "Darwin"
            else ()
        )

    for candidate in fallback_paths:
        path = candidate.expanduser()
        if path.is_file() and os.access(path, os.X_OK):
            return str(path)
    return None


def detect_clients(which=shutil.which) -> list[Client]:
    return [
        Client("codex", "Codex", resolve_executable("codex", which=which)),
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


def register_advisor_skill(source_root: Path, dry_run: bool, skills_dir=None):
    source = source_root / "skills" / "laya-model-advisor" / "SKILL.md"
    content = source.read_bytes()
    destination = (skills_dir or Path.home() / ".agents" / "skills") / "laya-model-advisor"
    target = destination / "SKILL.md"
    marker = destination / ".oh-my-laya.sha256"
    if destination.is_symlink() or target.is_symlink() or marker.is_symlink():
        raise RuntimeError(f"Refusing to replace a symlinked skill: {destination}")
    if destination.exists():
        if not target.is_file() or not marker.is_file():
            raise RuntimeError(f"Unmanaged skill already exists: {destination}")
        if hashlib.sha256(target.read_bytes()).hexdigest() != marker.read_text().strip():
            raise RuntimeError(f"Skill has local changes; preserve or move it first: {target}")
    print(f"+ install advisor skill at {destination}")
    if not dry_run:
        destination.mkdir(parents=True, exist_ok=True)
        target.write_bytes(content)
        marker.write_text(hashlib.sha256(content).hexdigest() + "\n")


ALPHA_SQUAD_REPOSITORY = "https://github.com/leo1394/skill-alpha-squad-coding-craft.git"
ALPHA_SQUAD_ARCHIVE = "https://codeload.github.com/leo1394/skill-alpha-squad-coding-craft/zip/HEAD"
ALPHA_SQUAD_PATH = Path("skills") / "alpha-squad-coding-craft"
MAX_ALPHA_ARCHIVE_BYTES = 20 * 1024 * 1024
MAX_ALPHA_EXTRACTED_BYTES = 100 * 1024 * 1024
MAX_ALPHA_FILES = 1000


def _alpha_manifest(source: Path):
    skill = source / "SKILL.md"
    if not skill.is_file() or skill.is_symlink():
        raise RuntimeError(f"Downloaded alpha skill is missing a regular SKILL.md: {source}")
    files = {}
    directories = []
    for path in sorted(source.rglob("*")):
        relative = path.relative_to(source).as_posix()
        if path.is_symlink():
            raise RuntimeError(f"Downloaded alpha skill contains a symlink: {path}")
        if path.is_dir():
            directories.append(relative)
        elif path.is_file():
            files[relative] = hashlib.sha256(path.read_bytes()).hexdigest()
        else:
            raise RuntimeError(f"Downloaded alpha skill contains an unsupported path: {path}")
    return {"directories": directories, "files": files}


def _read_alpha_manifest(marker: Path):
    try:
        manifest = json.loads(marker.read_text())
    except (OSError, ValueError):
        return None
    if not isinstance(manifest, dict):
        return None
    files = manifest.get("files")
    directories = manifest.get("directories")
    if not isinstance(files, dict) or not isinstance(directories, list):
        return None
    if (not all(isinstance(path, str) and isinstance(digest, str)
                for path, digest in files.items())
            or not all(isinstance(path, str) for path in directories)):
        return None
    return manifest


def _alpha_skill_state(destination: Path):
    marker = destination / ".oh-my-laya.manifest.json"
    if destination.is_symlink() or marker.is_symlink():
        return "protected", "destination or manifest is a symlink"
    if not destination.exists():
        return "absent", None
    if not destination.is_dir():
        return "protected", "destination is not a directory"
    if not marker.is_file():
        return "protected", "unmanaged skill already exists"
    manifest = _read_alpha_manifest(marker)
    if manifest is None:
        return "protected", "manifest is invalid or locally changed"

    expected_files = set(manifest["files"])
    expected_directories = set(manifest["directories"])
    actual_files = set()
    actual_directories = set()
    for path in destination.rglob("*"):
        relative = path.relative_to(destination).as_posix()
        if relative == marker.name:
            continue
        if path.is_symlink():
            return "protected", f"local symlink exists: {path}"
        if path.is_dir():
            actual_directories.add(relative)
        elif path.is_file():
            actual_files.add(relative)
        else:
            return "protected", f"unsupported local path exists: {path}"
    if actual_files != expected_files or actual_directories != expected_directories:
        return "protected", "skill has local files or removals"
    for relative, digest in manifest["files"].items():
        path = destination / relative
        if hashlib.sha256(path.read_bytes()).hexdigest() != digest:
            return "protected", f"skill has local changes: {path}"
    return "managed", manifest


def _write_alpha_skill(source: Path, destination: Path, manifest):
    for relative in manifest["directories"]:
        (destination / relative).mkdir(parents=True, exist_ok=True)
    for relative in manifest["files"]:
        (destination / relative).write_bytes((source / relative).read_bytes())
    (destination / ".oh-my-laya.manifest.json").write_text(
        json.dumps(manifest, indent=2, sort_keys=True) + "\n"
    )


def _safe_alpha_archive_extract(archive: Path, workspace: Path):
    source = workspace / "source"
    source.mkdir(parents=True)
    files = 0
    extracted_bytes = 0
    found = False
    with zipfile.ZipFile(archive) as bundle:
        for info in bundle.infolist():
            path = Path(info.filename)
            if path.is_absolute() or ".." in path.parts:
                raise RuntimeError("Alpha squad archive contains an unsafe path")
            if stat.S_ISLNK(info.external_attr >> 16):
                raise RuntimeError("Alpha squad archive contains a symlink")
            parts = path.parts
            try:
                index = parts.index("skills")
            except ValueError:
                continue
            if parts[index:index + 2] != ("skills", "alpha-squad-coding-craft"):
                continue
            relative = Path(*parts[index + 2:])
            if not relative.parts:
                found = True
                continue
            target = source / relative
            if info.is_dir():
                target.mkdir(parents=True, exist_ok=True)
                found = True
                continue
            files += 1
            extracted_bytes += info.file_size
            if files > MAX_ALPHA_FILES or extracted_bytes > MAX_ALPHA_EXTRACTED_BYTES:
                raise RuntimeError("Alpha squad archive exceeds extraction limits")
            target.parent.mkdir(parents=True, exist_ok=True)
            with bundle.open(info) as origin, target.open("wb") as destination:
                shutil.copyfileobj(origin, destination)
            found = True
    if not found or not (source / "SKILL.md").is_file():
        raise RuntimeError("Alpha squad archive is missing the required skill subtree")
    return source


def _download_alpha_squad_skill(workspace: Path):
    workspace.mkdir(parents=True, exist_ok=True)
    git = shutil.which("git")
    if git:
        checkout = workspace / "checkout"
        try:
            subprocess.run(
                [git, "clone", "--depth", "1", ALPHA_SQUAD_REPOSITORY, str(checkout)],
                check=True,
                text=True,
                capture_output=True,
            )
        except subprocess.CalledProcessError as error:
            detail = error.stderr.strip() or str(error)
            raise RuntimeError(f"Unable to clone latest alpha squad skill: {detail}") from error
        source = checkout / ALPHA_SQUAD_PATH
        ancestors = (checkout, checkout / "skills", source)
        if any(path.is_symlink() for path in ancestors):
            raise RuntimeError("Latest alpha squad repository contains a symlinked skill path")
        if not source.is_dir():
            raise RuntimeError("Latest alpha squad repository is missing the required skill subtree")
        return source

    archive = workspace / "alpha-squad.zip"
    try:
        with request.urlopen(ALPHA_SQUAD_ARCHIVE, timeout=30) as response, archive.open("wb") as output:
            while chunk := response.read(64 * 1024):
                if output.tell() + len(chunk) > MAX_ALPHA_ARCHIVE_BYTES:
                    raise RuntimeError("Alpha squad archive exceeds download limit")
                output.write(chunk)
    except OSError as error:
        raise RuntimeError(f"Unable to download latest alpha squad skill: {error}") from error
    try:
        return _safe_alpha_archive_extract(archive, workspace)
    except zipfile.BadZipFile as error:
        raise RuntimeError("Downloaded alpha squad archive is invalid") from error


def _install_alpha_skill_atomically(
    source: Path,
    destination: Path,
    manifest,
    expected_state,
    expected_manifest,
):
    destination.parent.mkdir(parents=True, exist_ok=True)
    staging = Path(tempfile.mkdtemp(prefix=".alpha-squad-staging-", dir=destination.parent))
    backup = None
    try:
        _write_alpha_skill(source, staging, manifest)
        current_state, current_manifest = _alpha_skill_state(destination)
        if (current_state != expected_state
                or (expected_state == "managed"
                    and current_manifest != expected_manifest)):
            raise RuntimeError(
                f"Alpha squad skill changed during download; refusing to replace: {destination}"
            )
        if expected_state == "managed":
            backup = Path(tempfile.mkdtemp(prefix=".alpha-squad-backup-", dir=destination.parent))
            backup.rmdir()
            os.replace(destination, backup)
        os.replace(staging, destination)
    except OSError as error:
        if backup is not None and backup.exists() and not destination.exists():
            try:
                os.replace(backup, destination)
            except OSError as rollback_error:
                raise RuntimeError(
                    f"Alpha squad install failed and rollback failed; backup retained at {backup}: "
                    f"{rollback_error}"
                ) from error
        raise
    finally:
        if staging.exists():
            shutil.rmtree(staging, ignore_errors=True)
    if backup is not None and backup.exists():
        state, _ = _alpha_skill_state(backup)
        if state == "managed":
            try:
                shutil.rmtree(backup)
            except OSError as error:
                print(f"! keep alpha squad backup at {backup}: {error}")


def register_alpha_squad_skill(
    dry_run: bool,
    codex_skills_dir=None,
    agents_skills_dir=None,
):
    codex_home = Path(os.environ.get("CODEX_HOME") or "~/.codex").expanduser()
    codex_skills = (
        Path(codex_skills_dir) if codex_skills_dir else codex_home / "skills"
    )
    agents_skills = (
        Path(agents_skills_dir)
        if agents_skills_dir else Path.home() / ".agents" / "skills"
    )
    candidates = list(dict.fromkeys([
        codex_skills / "alpha-squad-coding-craft",
        agents_skills / "alpha-squad-coding-craft",
    ]))
    states = [
        (destination, *_alpha_skill_state(destination))
        for destination in candidates
    ]
    existing = [item for item in states if item[1] != "absent"]
    if len(existing) > 1:
        locations = ", ".join(str(item[0]) for item in existing)
        print(f"! skip alpha squad skill: duplicate existing locations: {locations}")
        return False
    destination, state, detail = existing[0] if existing else (candidates[0], "absent", None)
    if state == "protected":
        print(f"! skip alpha squad skill at {destination}: {detail}")
        return False
    action = "update" if state == "managed" else "install"
    print(f"+ {action} alpha squad skill at {destination}")
    if dry_run:
        return True
    print("+ fetch latest alpha squad skill")
    with tempfile.TemporaryDirectory(prefix="oh-my-laya-alpha-") as temporary:
        source = _download_alpha_squad_skill(Path(temporary))
        manifest = _alpha_manifest(source)
        _install_alpha_skill_atomically(
            source, destination, manifest, state, detail
        )
    return True


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


def register_client_skills(source_root, dry_run, client):
    if client == "codex":
        register_advisor_skill(source_root, dry_run)
        return register_alpha_squad_skill(dry_run)
    root = client_home(client) / "skills"
    register_advisor_skill(source_root, dry_run, root)
    return register_alpha_squad_skill(dry_run, root, root)


def configure_goal(client, dry_run):
    if client == "codex":
        return install_goal_workflow(dry_run=dry_run)
    return install_goal_workflow(dry_run=dry_run, client=client)


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
                "laya_tell_me_agent.download",
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
    parser.add_argument(
        "--goal-workflow", choices=("yes", "no"),
        help="opt in/out of /goal rules for every selected client; otherwise ask per client",
    )
    parser.add_argument(
        "--advice-policy", choices=POLICIES,
        help="Codex recommendations: always ask, conditional ask, or auto accept",
    )
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
    if args.advice_policy and "codex" not in targets:
        raise ValueError("--advice-policy requires the codex target")
    goal_targets = set()
    for target in targets:
        if select_goal_workflow(args.goal_workflow, args.dry_run, client=target):
            goal_targets.add(target)
            configure_goal(target, True)
        # Validate ownership before downloading or changing client registrations.
        register_client_skills(source_root, True, target)
    server, model_dir = install_runtime(
        source_root, install_dir, args.model, args.dry_run
    )
    by_key = {client.key: client for client in clients}

    for target in targets:
        executable = by_key[target].executable
        if target == "codex":
            register_codex(executable, server, model_dir, args.dry_run)
            if args.advice_policy:
                print(f"+ set model advice policy: {args.advice_policy}")
                if not args.dry_run:
                    preferences(args.advice_policy)
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
        register_client_skills(source_root, args.dry_run, target)
        if target in goal_targets:
            configure_goal(target, args.dry_run)

    print()
    print(f"Installed model: {args.model}")
    print(f"Registered clients: {', '.join(targets)}")
    print("Restart active agent sessions so they refresh their tool lists.")


if __name__ == "__main__":
    main()
