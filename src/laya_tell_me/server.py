import json
import os
import threading
from typing import Any

from mcp.server import MCPServer

from .advisor import ADVISOR_QUESTIONS, build_advice, preferences, validate_catalog, validate_ceiling, validate_squad
from .routing import ROLES, build_role_advice, pair_supported


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
    questions: dict[str, dict[str, Any]] | None = None,
    advisor: dict[str, Any] | None = None,
) -> dict[str, Any]:
    """Run a local typed decision with Laya-MLX.

    Use for bounded classification, ordered scoring, and yes/no probability.
    Question types are choice, score, and noul. Treat results as advisory;
    never use them as authorization for destructive or consequential actions.

    For model advice, omit questions and pass advisor with models and current_model.
    Each host-verified model has id, reasoning_efforts (least to most), and an
    optional user-approved tier: fast, balanced, strong. No model is switched.
    For Alpha Squad, also pass role and verified current_reasoning_effort.
    Non-auto execution roles also require the confirmed session execution_choice
    pair; omit it for auto and reviewer routing.
    Returned delegation parameters are recommendations for the host's spawn
    call, never execution authorization. The current session is not changed.
    """
    if advisor is not None:
        if questions is not None:
            raise ValueError("advisor mode uses fixed questions; omit questions")
        validate_catalog(advisor.get("models", []))
        if "role" in advisor and advisor["role"] not in ROLES:
            raise ValueError(f"advisor role must be one of {ROLES}")
        questions = ADVISOR_QUESTIONS
    if not questions:
        raise ValueError("questions must contain at least one question")
    if len(questions) > 64:
        raise ValueError("at most 64 questions are accepted per call")
    if len(json.dumps(state, ensure_ascii=False)) > 256_000:
        raise ValueError("state is too large; summarize it before calling Laya")

    result = get_agent().predict(state, questions)
    if advisor is not None:
        settings = preferences()
        if "role" in advisor:
            advice = build_role_advice(
                result, advisor.get("models", []), advisor.get("current_model"),
                advisor.get("current_reasoning_effort"), advisor["role"], settings,
                advisor.get("execution_choice"),
            )
        else:
            advice = build_advice(
                result, advisor.get("models", []), advisor.get("current_model"), settings,
            )
        return {
            "laya_result": result,
            "advice": advice,
        }
    return result


@mcp.tool()
def laya_advisor_preferences(
    policy: str | None = None,
    ceiling: dict[str, str] | None = None,
    models: list[dict[str, Any]] | None = None,
    squad: dict[str, Any] | None = None,
) -> dict[str, Any]:
    """Read preferences, or save ONLY after the user's Confirm and continue.

    always: ask on every assessment; conditional: ask only for high complexity,
    high risk or uncertainty; auto: accept within the user-approved ceiling.
    For auto, provide ceiling={model, reasoning_effort} and the host-verified,
    locally filtered models catalog. Auto uses only that model and at most that
    effort. Missing ceilings require setup; no automatic acceptance is allowed.
    These authorize recommendations, not actual switching or any other action.
    Omit policy to read. Unknown/unconfigured preferences default to asking.
    Optional squad={enabled: bool, reviewer: {model, reasoning_effort} or null}
    enables Alpha Squad's bounded spawn recommendations after explicit setup.
    Null reviewer uses the orchestrator; difficult reviews always use it.
    Omitted squad preserves its prior setting. This never authorizes task actions.
    """
    if ceiling is not None:
        validate_ceiling(ceiling)
        catalog = validate_catalog(models)
        model = next((item for item in catalog if item["id"] == ceiling["model"]), None)
        if model is None or ceiling["reasoning_effort"] not in model["reasoning_efforts"]:
            raise ValueError("ceiling is not in the host-verified, locally allowed catalog")
    if squad is not None:
        validate_squad(squad)
        if squad["reviewer"] is not None and not pair_supported(
            squad["reviewer"], validate_catalog(models)
        ):
            raise ValueError("reviewer is not in the host-verified, locally allowed catalog")
    return preferences(policy, ceiling=ceiling, squad=squad)


def main():
    mcp.run()


if __name__ == "__main__":
    main()
