#!/usr/bin/env python3
"""Run the published installer in an isolated home without Node or Rust tools."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import signal
import subprocess
import sys
import tempfile


BOOTSTRAP_URL = (
    "https://raw.githubusercontent.com/leo1394/oh-my-laya/master/"
    "tools/oh-my-laya.sh"
)
PYTHON = Path("/opt/homebrew/bin/python3")
CODEX = Path(
    "/Applications/ChatGPT.app/Contents/Resources/codex-cli/"
    "CodexCLI.app/Contents/MacOS/codex"
)
TOOLS = {
    "bash": Path("/bin/bash"),
    "curl": Path("/usr/bin/curl"),
    "dirname": Path("/usr/bin/dirname"),
    "gzip": Path("/usr/bin/gzip"),
    "mkdir": Path("/bin/mkdir"),
    "mktemp": Path("/usr/bin/mktemp"),
    "rm": Path("/bin/rm"),
    "sh": Path("/bin/sh"),
    "tar": Path("/usr/bin/tar"),
    "uname": Path("/usr/bin/uname"),
}
FORBIDDEN = ("node", "npm", "npx", "cargo", "rustc", "rustup")


def terminate_group(process):
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        return process.communicate()
    try:
        output = process.communicate(timeout=5)
    except subprocess.TimeoutExpired:
        output = (None, None)
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except (ProcessLookupError, PermissionError):
        pass
    if process.poll() is None:
        output = process.communicate()
    return output


def display_output(value, stream, *, limit=65536):
    if not value:
        return
    if len(value) > limit:
        half = limit // 2
        value = value[:half] + "\n... output truncated ...\n" + value[-half:]
    print(value, end="" if value.endswith("\n") else "\n", file=stream)


def run(command, environment, *, input_text=None, timeout=60, echo=True):
    print("+", " ".join(str(part) for part in command), flush=True)
    process = subprocess.Popen(
        [str(part) for part in command], env=environment,
        text=True, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        start_new_session=True,
    )
    try:
        stdout, stderr = process.communicate(input=input_text, timeout=timeout)
    except subprocess.TimeoutExpired as error:
        stdout, stderr = terminate_group(process)
        display_output(stdout, sys.stdout)
        display_output(stderr, sys.stderr)
        raise RuntimeError(
            f"command timed out after {timeout}s: "
            + " ".join(str(part) for part in command)
        ) from error
    except BaseException:
        terminate_group(process)
        raise
    terminate_group(process)
    if echo:
        display_output(stdout, sys.stdout)
        display_output(stderr, sys.stderr)
    if process.returncode:
        if not echo:
            display_output(stdout, sys.stdout)
            display_output(stderr, sys.stderr)
        raise RuntimeError(
            f"command exited {process.returncode}: "
            + " ".join(str(part) for part in command)
        )
    return stdout


def isolated_environment(root, python, codex):
    home = root / "home"
    temporary = root / "tmp"
    binaries = root / "bin"
    codex_home = home / ".codex"
    for directory in (home, temporary, binaries, codex_home):
        directory.mkdir()
    tools = {**TOOLS, "python3": python, "codex": codex}
    for name, source in tools.items():
        if not source.is_file():
            raise RuntimeError(f"required executable is missing: {source}")
        (binaries / name).symlink_to(source)
    environment = {
        "CODEX_HOME": str(codex_home),
        "HOME": str(home),
        "LANG": os.environ.get("LANG", "en_US.UTF-8"),
        "LOGNAME": os.environ.get("LOGNAME", os.environ.get("USER", "tester")),
        "NO_COLOR": "1",
        "PATH": str(binaries),
        "PIP_CACHE_DIR": str(root / "pip-cache"),
        "PYTHONNOUSERSITE": "1",
        "SHELL": "/bin/sh",
        "TERM": "dumb",
        "TMPDIR": str(temporary),
        "USER": os.environ.get("USER", "tester"),
        "XDG_CACHE_HOME": str(root / "xdg-cache"),
        "HF_HOME": str(root / "huggingface"),
    }
    for name in FORBIDDEN:
        if shutil.which(name, path=environment["PATH"]) is not None:
            raise RuntimeError(f"forbidden tool is visible in isolated PATH: {name}")
    return environment


def python_runtime(install_dir):
    candidates = sorted((install_dir / "runtimes").glob("python-*/bin/python"))
    if len(candidates) != 1:
        raise RuntimeError(f"expected one Python runtime, found: {candidates}")
    return candidates[0]


def verify_manifest(install_dir, environment):
    python = python_runtime(install_dir)
    code = (
        "import json; from laya_tell_me.runtime_install import "
        "TRUSTED_RELEASES, WORKBENCH_VERSION; "
        "print(json.dumps({'version': WORKBENCH_VERSION, "
        "'release': TRUSTED_RELEASES.get(WORKBENCH_VERSION)}))"
    )
    payload = json.loads(run([python, "-c", code], environment).strip())
    release = payload.get("release")
    if not isinstance(release, dict):
        raise RuntimeError(f"installed package has no trusted release: {payload}")
    expected = release.get("sha256")
    if not isinstance(expected, str) or len(expected) != 64:
        raise RuntimeError(f"installed package has an invalid release digest: {release}")
    runtimes = sorted((install_dir / "runtimes").glob("workbench-*/laya"))
    if len(runtimes) != 1:
        raise RuntimeError(f"expected one Workbench runtime, found: {runtimes}")
    actual = hashlib.sha256(runtimes[0].read_bytes()).hexdigest()
    if actual != expected:
        raise RuntimeError(f"Workbench digest mismatch: expected {expected}, got {actual}")
    print(f"verified release manifest: {payload['version']} {actual}")


def verify_mcp(server, environment):
    messages = (
        {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-11-25", "capabilities": {},
            "clientInfo": {"name": "published-install-check", "version": "1"},
        }},
        {"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}},
        {"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}},
    )
    output = run(
        [server], environment,
        input_text="".join(json.dumps(message) + "\n" for message in messages),
        echo=False,
    )
    replies = [json.loads(line) for line in output.splitlines() if line.strip()]
    initialization = next(reply for reply in replies if reply.get("id") == 1)
    tools = next(reply for reply in replies if reply.get("id") == 2)
    if initialization["result"]["serverInfo"]["name"] != "oh-my-laya":
        raise RuntimeError(f"unexpected MCP server info: {initialization}")
    names = {item["name"] for item in tools["result"]["tools"]}
    expected = {"laya_tell_me", "laya_advisor_preferences", "laya_feedback"}
    if not expected.issubset(names):
        raise RuntimeError(f"missing MCP tools: {sorted(expected - names)}")
    print("verified MCP discovery:", ", ".join(sorted(names)))


def status(command, environment):
    return json.loads(run(
        [command, "status"], environment, timeout=10, echo=False,
    ))


def service_running(payload):
    if payload.get("running") is False and payload.get("state") == "not_listening":
        return False
    service = payload.get("service")
    if payload.get("ok") is True and isinstance(service, dict):
        pid = service.get("pid")
        if isinstance(pid, int) and pid > 0:
            return True
    raise RuntimeError(f"unable to determine service state: {payload}")


def verify_idle_status(payload):
    if not service_running(payload):
        return
    settings = payload.get("settings", {})
    worker = payload.get("worker", {})
    if settings.get("recording_enabled") is not False:
        raise RuntimeError(f"recording is not disabled: {settings}")
    if (worker.get("pid") != 0 or worker.get("queued") != 0
            or worker.get("busy") is not False):
        raise RuntimeError(f"model worker is not idle: {worker}")


def verify_status_shapes():
    verify_idle_status({"running": False, "state": "not_listening"})
    verify_idle_status({
        "ok": True,
        "service": {"pid": 123},
        "settings": {"recording_enabled": False},
        "worker": {"pid": 0, "queued": 0, "busy": False},
    })
    try:
        service_running({"ok": True})
    except RuntimeError:
        pass
    else:
        raise RuntimeError("ambiguous service status was accepted")


def relevant_lines(output):
    return "\n".join(
        line for line in output.splitlines()
        if "oh-my-laya" in line.lower() or "oh my laya" in line.lower()
    )


def seed_model(source, destination, environment):
    source = source.expanduser().resolve()
    if not (source / "model.safetensors").is_file():
        raise RuntimeError(f"seed model is incomplete: {source}")
    destination.parent.mkdir(parents=True, exist_ok=True)
    run(["/bin/cp", "-cR", source, destination], environment, timeout=120)
    source_weight = source / "model.safetensors"
    destination_weight = destination / "model.safetensors"
    if not destination_weight.is_file():
        raise RuntimeError(f"seed copy is incomplete: {destination}")
    if source_weight.samefile(destination_weight):
        raise RuntimeError("seed copy shares an inode with the source model")
    print(f"seeded independent copy-on-write model: {source} -> {destination}")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--keep", action="store_true", help="retain the isolated root")
    parser.add_argument(
        "--keep-on-failure", action="store_true",
        help="retain the isolated root after a failed check",
    )
    parser.add_argument("--seed-model", type=Path)
    parser.add_argument("--python", type=Path, default=PYTHON)
    parser.add_argument("--codex", type=Path, default=CODEX)
    args = parser.parse_args()
    if platform.system() != "Darwin" or platform.machine() != "arm64":
        raise RuntimeError("this acceptance check requires Apple Silicon macOS")

    root = Path(tempfile.mkdtemp(prefix="oh-my-laya-published-", dir="/private/tmp"))
    print(f"isolated root: {root}")
    environment = None
    command = root / "home" / ".local" / "bin" / "laya"
    safe_to_clean = True
    succeeded = False
    try:
        verify_status_shapes()
        python = args.python.expanduser().resolve()
        codex = args.codex.expanduser().resolve()
        environment = isolated_environment(root, python, codex)
        print("isolated PATH:", environment["PATH"])
        print("forbidden tools absent:", ", ".join(FORBIDDEN))
        run([python, "--version"], environment)
        run([codex, "--version"], environment)

        if args.seed_model is not None:
            seed_model(
                args.seed_model,
                root / "home" / ".local" / "share" / "oh-my-laya"
                / "models" / "multilingual",
                environment,
            )

        bootstrap = root / "oh-my-laya.sh"
        run([root / "bin" / "curl", "-fsSL", BOOTSTRAP_URL, "-o", bootstrap], environment)
        run([
            root / "bin" / "sh", bootstrap, "--targets", "codex",
            "--goal-workflow", "no",
        ], environment, timeout=1800)

        home = root / "home"
        install_dir = home / ".local" / "share" / "oh-my-laya"
        server = install_dir / "bin" / "oh-my-laya-mcp"
        version = run([command, "--version"], environment).strip()
        if version != "laya 0.2.0":
            raise RuntimeError(f"unexpected installed version: {version!r}")
        verify_idle_status(status(command, environment))
        verify_manifest(install_dir, environment)
        verify_mcp(server, environment)
        plugin = run([codex, "plugin", "list"], environment, echo=False)
        if "oh-my-laya" not in plugin.lower() and "oh my laya" not in plugin.lower():
            raise RuntimeError("Codex plugin list does not contain Oh My Laya")
        print("Codex plugin discovery:", relevant_lines(plugin))
        mcp = run(
            [codex, "mcp", "get", "oh-my-laya"], environment, echo=False,
        )
        if "oh-my-laya" not in mcp.lower() and "oh my laya" not in mcp.lower():
            raise RuntimeError("Codex MCP discovery does not contain Oh My Laya")
        print("Codex MCP discovery:", relevant_lines(mcp))
        succeeded = True
        print("PASS: published isolated installation and discovery checks succeeded")
    finally:
        if environment is not None and command is not None and command.exists():
            try:
                before = status(command, environment)
                if service_running(before):
                    run([command, "stop"], environment, timeout=10, echo=False)
                    after = status(command, environment)
                    if service_running(after):
                        raise RuntimeError(f"isolated service did not stop: {after}")
            except BaseException as error:
                safe_to_clean = False
                print(f"unable to establish isolated service shutdown: {error}", file=sys.stderr)
        if args.keep or (args.keep_on_failure and not succeeded) or not safe_to_clean:
            print(f"retained isolated root: {root}")
        else:
            shutil.rmtree(root)
            print(f"removed isolated root: {root}")


if __name__ == "__main__":
    main()
