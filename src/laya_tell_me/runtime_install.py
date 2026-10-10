import hashlib
import os
import shlex
import shutil
import stat
import subprocess
import tempfile
from pathlib import Path
from urllib import request


WORKBENCH_VERSION = "2.0.1"
# Release artifact bytes are verified before publication and pinned here.
# Never replace an existing asset; publish a new tag when its bytes change.
TRUSTED_RELEASES = {
    "2.0.1": {
        "url": "https://github.com/leo1394/oh-my-laya/releases/download/v2.0.1/laya-macos-arm64",
        "sha256": "ae65a3c61473c121271282eaf904dbaf7b8d52cd507d79e3cc26c885a4b3558f",
    },
    "0.2.0": {
        "url": "https://github.com/leo1394/oh-my-laya/releases/download/v0.2.0-rc.3/laya-macos-arm64",
        "sha256": "15316d19599fb2df80436a4250bd0a516dadca0454f5a196f8beb532b4486854",
    },
}
LAUNCHER_MARKER = "# Managed by oh-my-laya: workbench launcher v1\n"
STDIO_MARKER = "# Managed by oh-my-laya: workbench stdio v1\n"
LEGACY_LAUNCHER_MARKER = "# Managed by oh-my-laya: snake launcher v1\n"


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _download_release(destination: Path, *, urlopen=request.urlopen) -> None:
    release = TRUSTED_RELEASES.get(WORKBENCH_VERSION)
    if not release:
        raise RuntimeError(
            f"No trusted prebuilt Laya Workbench {WORKBENCH_VERSION} artifact is pinned. "
            "Build crates/laya locally and rerun with --workbench-binary /absolute/path/to/laya."
        )
    url = release.get("url")
    expected = release.get("sha256")
    if not url or not expected or len(expected) != 64:
        raise RuntimeError(f"Trusted release manifest for {WORKBENCH_VERSION} is incomplete")
    with urlopen(url, timeout=30) as response, destination.open("wb") as output:
        shutil.copyfileobj(response, output)
    actual = _sha256(destination)
    if actual != expected:
        raise RuntimeError(
            f"Laya Workbench checksum mismatch: expected {expected}, got {actual}"
        )


def _validate_binary(path: Path, *, run=subprocess.run) -> None:
    if not path.is_file() or path.is_symlink():
        raise RuntimeError(f"Workbench binary is not a regular file: {path}")
    path.chmod(path.stat().st_mode | 0o700)
    try:
        result = run(
            [str(path), "--version"], check=True, capture_output=True,
            text=True, timeout=10,
        )
    except (OSError, subprocess.SubprocessError) as error:
        raise RuntimeError(f"Unable to execute Laya Workbench binary {path}: {error}") from error
    if result.stdout.strip() != f"laya {WORKBENCH_VERSION}":
        raise RuntimeError(
            f"Unexpected Laya Workbench version from {path}: {result.stdout.strip()!r}; "
            f"expected 'laya {WORKBENCH_VERSION}'"
        )


def install_binary(install_dir: Path, source: Path | None, *, urlopen=request.urlopen) -> Path:
    runtimes = install_dir / "runtimes"
    runtimes.mkdir(parents=True, exist_ok=True)
    runtime = Path(tempfile.mkdtemp(prefix=f"workbench-{WORKBENCH_VERSION}-", dir=runtimes))
    binary = runtime / "laya"
    try:
        if source is None:
            _download_release(binary, urlopen=urlopen)
        else:
            source = source.expanduser().resolve()
            if not source.is_file() or source.is_symlink():
                raise RuntimeError(f"Workbench binary is not a regular file: {source}")
            shutil.copyfile(source, binary)
        _validate_binary(binary)
        return binary
    except BaseException:
        shutil.rmtree(runtime, ignore_errors=True)
        raise


def _managed_state(path: Path, markers: tuple[str, ...]) -> str:
    if path.is_symlink():
        return "unmanaged"
    if not path.exists():
        return "absent"
    if not path.is_file():
        return "unmanaged"
    try:
        prefix = path.read_text()[:512]
    except (OSError, UnicodeDecodeError):
        return "unmanaged"
    return "managed" if any(prefix.startswith("#!/bin/sh\n" + marker) for marker in markers) else "unmanaged"


def _legacy_python_stdio(path: Path) -> bool:
    if path.is_symlink() or not path.is_file():
        return False
    try:
        content = path.read_text()
    except (OSError, UnicodeDecodeError):
        return False
    entrypoint = ("from laya_tell_me.server import main" in content
                  or "from laya_tell_me_agent.server import main" in content)
    return (content.startswith("#!") and entrypoint
            and path.parent.name == "bin" and path.parent.parent.name == "venv")


def _stdio_target(install_dir: Path) -> Path:
    stable = install_dir / "bin" / "oh-my-laya-mcp"
    legacy = install_dir / "venv" / "bin" / "oh-my-laya-mcp"
    if stable.exists() or stable.is_symlink():
        return stable
    if legacy.exists() or legacy.is_symlink():
        return legacy
    return stable


def validate_launcher_ownership(install_dir: Path, *, bin_dir: Path | None = None) -> None:
    command = (bin_dir or Path.home() / ".local" / "bin") / "laya"
    if _managed_state(command, (LAUNCHER_MARKER, LEGACY_LAUNCHER_MARKER)) == "unmanaged":
        raise RuntimeError(f"Refusing to overwrite an existing unmanaged launcher: {command}")
    stdio = _stdio_target(install_dir)
    state = _managed_state(stdio, (STDIO_MARKER,))
    if state == "unmanaged" and not _legacy_python_stdio(stdio):
        raise RuntimeError(f"Refusing to overwrite an existing unmanaged launcher: {stdio}")


def _environment(binary: Path, python: Path, model_dir: Path, workbench_dir: Path) -> str:
    values = {
        "LAYA_PYTHON": python,
        "LAYA_MODEL_DIR": model_dir,
        "LAYA_WORKBENCH_DIR": workbench_dir,
        "LAYA_SNAKE_BIN": python.parent / "laya-snake",
    }
    exports = "".join(
        f"export {name}={shlex.quote(str(value))}\n" for name, value in values.items()
    )
    return exports + f"exec {shlex.quote(str(binary))} \"$@\"\n"


def _snapshot(path: Path):
    if not path.exists():
        return None
    return path.read_bytes(), stat.S_IMODE(path.stat().st_mode)


def _restore(path: Path, snapshot) -> None:
    if snapshot is None:
        path.unlink(missing_ok=True)
        return
    descriptor, name = tempfile.mkstemp(prefix=f".{path.name}-restore-", dir=path.parent)
    temporary = Path(name)
    try:
        with os.fdopen(descriptor, "wb") as stream:
            stream.write(snapshot[0])
        temporary.chmod(snapshot[1])
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def _prepare_wrapper(target: Path, marker: str, content: str) -> Path:
    target.parent.mkdir(parents=True, exist_ok=True)
    descriptor, name = tempfile.mkstemp(prefix=f".{target.name}-", dir=target.parent)
    temporary = Path(name)
    with os.fdopen(descriptor, "w") as stream:
        stream.write("#!/bin/sh\n" + marker + content)
    temporary.chmod(0o755)
    subprocess.run(["/bin/sh", "-n", str(temporary)], check=True, capture_output=True)
    return temporary


def install_launchers(
    install_dir: Path,
    binary: Path,
    python: Path,
    model_dir: Path,
    *,
    bin_dir: Path | None = None,
) -> tuple[Path, Path]:
    validate_launcher_ownership(install_dir, bin_dir=bin_dir)
    command = (bin_dir or Path.home() / ".local" / "bin") / "laya"
    stdio = _stdio_target(install_dir)
    environment = _environment(binary, python, model_dir, install_dir / "workbench")
    stdio_content = environment.replace(
        f'exec {shlex.quote(str(binary))} "$@"',
        f'exec {shlex.quote(str(binary))} mcp "$@"',
    )
    specs = (
        (command, LAUNCHER_MARKER, environment),
        (stdio, STDIO_MARKER, stdio_content),
    )
    snapshots = {target: _snapshot(target) for target, _, _ in specs}
    rollback = {}
    temporary = []
    changed = []
    try:
        for target, marker, content in specs:
            temporary.append((target, _prepare_wrapper(target, marker, content)))
        for target, _marker, _content in specs:
            state = snapshots[target]
            if state is not None:
                descriptor, name = tempfile.mkstemp(
                    prefix=f".{target.name}-rollback-", dir=target.parent
                )
                with os.fdopen(descriptor, "wb") as stream:
                    stream.write(state[0])
                rollback[target] = Path(name)
                rollback[target].chmod(state[1])
            legacy = ((target == command and _managed_state(target, (LEGACY_LAUNCHER_MARKER,)) == "managed")
                      or (target == stdio and _legacy_python_stdio(target)))
            backup = target.with_name(target.name + ".oh-my-laya-legacy-backup")
            if legacy and state is not None and not backup.exists():
                backup.write_bytes(state[0])
                backup.chmod(state[1])
        for target, staged in temporary:
            os.replace(staged, target)
            changed.append(target)
        _validate_binary(binary)
        subprocess.run([str(command), "--version"], check=True, capture_output=True, timeout=10)
        return command, stdio
    except BaseException:
        restore_error = None
        for target in reversed(changed):
            try:
                if snapshots[target] is None:
                    target.unlink(missing_ok=True)
                else:
                    os.replace(rollback[target], target)
                    rollback.pop(target)
            except BaseException as error:
                restore_error = restore_error or error
        if restore_error is not None:
            raise RuntimeError(f"Launcher install failed and rollback failed: {restore_error}")
        raise
    finally:
        for _target, staged in temporary:
            staged.unlink(missing_ok=True)
        for backup in rollback.values():
            backup.unlink(missing_ok=True)


def install_wrapper(
    target: Path,
    content: str,
    marker: str,
    *,
    accepted_markers: tuple[str, ...] = (),
    accept_legacy_stdio: bool = False,
) -> Path:
    state = _managed_state(target, (marker,) + accepted_markers)
    if state == "unmanaged" and not (accept_legacy_stdio and _legacy_python_stdio(target)):
        raise RuntimeError(f"Refusing to overwrite an existing unmanaged launcher: {target}")
    snapshot = _snapshot(target)
    staged = _prepare_wrapper(target, marker, content)
    try:
        os.replace(staged, target)
        return target
    except BaseException:
        _restore(target, snapshot)
        raise
    finally:
        staged.unlink(missing_ok=True)
