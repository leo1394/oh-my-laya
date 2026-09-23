import argparse
import hashlib
from pathlib import Path

from .models import MODELS


REQUIRED_FILES = (
    "model.safetensors",
    "rl_agent_config.json",
    "encoder/config.json",
    "tokenizer/tokenizer.json",
)


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def is_valid_model(path: Path, expected_sha256: str) -> bool:
    return all((path / name).is_file() for name in REQUIRED_FILES) and sha256_file(
        path / "model.safetensors"
    ) == expected_sha256


def download_model(model: str, destination: Path) -> Path:
    from huggingface_hub import snapshot_download

    spec = MODELS[model]
    destination = destination.expanduser().resolve()

    if is_valid_model(destination, spec.weight_sha256):
        print(f"Model already verified: {destination}")
        return destination

    destination.mkdir(parents=True, exist_ok=True)
    snapshot_download(
        repo_id=spec.repo,
        revision=spec.revision,
        local_dir=destination,
    )

    missing = [name for name in REQUIRED_FILES if not (destination / name).is_file()]
    if missing:
        raise RuntimeError(f"Downloaded checkpoint is incomplete: {', '.join(missing)}")

    actual = sha256_file(destination / "model.safetensors")
    if actual != spec.weight_sha256:
        raise RuntimeError(
            "Downloaded model checksum mismatch: "
            f"expected {spec.weight_sha256}, got {actual}"
        )

    print(f"Downloaded and verified {spec.repo}@{spec.revision}")
    return destination


def main(argv=None):
    parser = argparse.ArgumentParser(description="Download a pinned Laya-MLX checkpoint")
    parser.add_argument("--model", choices=sorted(MODELS), default="multilingual")
    parser.add_argument("--destination", type=Path, required=True)
    args = parser.parse_args(argv)
    download_model(args.model, args.destination)


if __name__ == "__main__":
    main()
