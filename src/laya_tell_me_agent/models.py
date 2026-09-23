from dataclasses import dataclass


@dataclass(frozen=True)
class ModelSpec:
    repo: str
    revision: str
    weight_sha256: str


MODELS = {
    "english": ModelSpec(
        repo="aac6fef/laya-mlx",
        revision="047678560251f28113ee8f5df4be82102c7bf336",
        weight_sha256="b9c07bf14be2fa5c78a9193a3e6d840ac80e89e62fc40f425834c3d8a6eaa3de",
    ),
    "multilingual": ModelSpec(
        repo="aac6fef/laya-multilingual-mlx",
        revision="ba40c87fcb357f1643d04d71323af9cdc3b9e591",
        weight_sha256="7fc5834af4d8fdfb268d272a9d1a66e5819a0daac98241651c4c888cc43adff1",
    ),
    "typed-decisions": ModelSpec(
        repo="aac6fef/laya-typed-decisions-mlx",
        revision="28416e78cb26a239a4eabaa2e084904ec5e6cacb",
        weight_sha256="804ef8802b4cac7a67913b0cfb8448659e934a50284aaa867b98d7d9a6e7d1e0",
    ),
}

