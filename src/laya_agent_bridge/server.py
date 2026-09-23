import json
import os
import threading
from typing import Any

from mcp.server import MCPServer


mcp = MCPServer("oh-my-laya")
_agent = None
_agent_lock = threading.Lock()


def get_agent():
    global _agent

    if _agent is not None:
        return _agent

    with _agent_lock:
        if _agent is None:
            model_dir = os.environ.get("LAYA_MODEL_DIR")
            if not model_dir:
                raise RuntimeError("LAYA_MODEL_DIR is not configured")

            import laya_mlx

            _agent = laya_mlx.load(
                model_dir,
                dtype=os.environ.get("LAYA_DTYPE", "float16"),
                device=os.environ.get("LAYA_DEVICE", "gpu"),
                batch_size=int(os.environ.get("LAYA_BATCH_SIZE", "16")),
            )

    return _agent


@mcp.tool()
def laya_tell_me(
    state: str | dict[str, Any] | list[Any],
    questions: dict[str, dict[str, Any]],
) -> dict[str, Any]:
    """Run a local typed decision with Laya-MLX.

    Use for bounded classification, ordered scoring, and yes/no probability.
    Question types are choice, score, and noul. Treat results as advisory;
    never use them as authorization for destructive or consequential actions.
    """
    if not questions:
        raise ValueError("questions must contain at least one question")
    if len(questions) > 64:
        raise ValueError("at most 64 questions are accepted per call")
    if len(json.dumps(state, ensure_ascii=False)) > 256_000:
        raise ValueError("state is too large; summarize it before calling Laya")

    return get_agent().predict(state, questions)


def main():
    mcp.run()


if __name__ == "__main__":
    main()
