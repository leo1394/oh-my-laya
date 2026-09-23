"""Advisory model routing; never changes a host's model or execution permissions."""

import json
import math
import os
from pathlib import Path
import tempfile


POLICIES = ("always", "conditional", "auto")
CONFIDENCE_THRESHOLD = 0.6
ADVISOR_QUESTIONS = {
    "complexity": {
        "type": "choice",
        "instructions": "Classify the task's implementation complexity.",
        "criteria": {
            "low": "Small, clear, localized change",
            "medium": "Several related changes with known requirements",
            "high": "Architecture, difficult debugging, or broad dependencies",
        },
    },
    "risk": {
        "type": "choice",
        "instructions": "Classify the consequences of an incorrect change.",
        "criteria": {
            "low": "Documentation or easily reversible isolated change",
            "medium": "Behavior change requiring regression tests",
            "high": "Security, data loss, production or core functionality",
        },
    },
    "certainty": {
        "type": "choice",
        "instructions": "Is there enough information to assess this task?",
        "criteria": {
            "clear": "Requirements and impact are sufficiently clear",
            "uncertain": "Missing context, conflicting requirements or unknown impact",
        },
    },
}


def preferences_path():
    return Path(os.environ.get(
        "LAYA_ADVISOR_CONFIG", "~/.config/oh-my-laya/advisor.json"
    )).expanduser()


def validate_ceiling(ceiling):
    if not isinstance(ceiling, dict) or set(ceiling) != {"model", "reasoning_effort"}:
        raise ValueError("ceiling requires model and reasoning_effort")
    if any(not isinstance(value, str) or not value.strip() for value in ceiling.values()):
        raise ValueError("ceiling model and reasoning_effort must be nonempty strings")
    return ceiling


def validate_squad(squad):
    if not isinstance(squad, dict) or set(squad) != {"enabled", "reviewer"}:
        raise ValueError("squad requires enabled and reviewer")
    if type(squad["enabled"]) is not bool:
        raise ValueError("squad enabled must be a boolean")
    if squad["reviewer"] is not None:
        validate_ceiling(squad["reviewer"])
    return squad


def preferences(policy=None, path=None, *, ceiling=None, squad=None):
    path = path or preferences_path()
    try:
        saved = json.loads(path.read_text())
        if not isinstance(saved, dict):
            saved = {}
    except (OSError, ValueError):
        saved = {}
    if squad is not None:
        validate_squad(squad)
    if ceiling is not None:
        validate_ceiling(ceiling)
        if policy != "auto":
            raise ValueError("ceiling can only be saved with auto policy")
    if policy is not None or squad is not None:
        if policy is None:
            policy = saved.get("policy", "always")
            ceiling = saved.get("ceiling") if policy == "auto" else None
        if policy not in POLICIES:
            raise ValueError(f"policy must be one of {POLICIES}")
        saved.update({"policy": policy, "ceiling": ceiling})
        if squad is not None:
            saved["squad"] = squad
        path.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.NamedTemporaryFile(
            mode="w", dir=path.parent, delete=False, encoding="utf-8"
        ) as stream:
            temporary = Path(stream.name)
            json.dump(saved, stream)
            stream.write("\n")
        try:
            temporary.replace(path)
        finally:
            temporary.unlink(missing_ok=True)
    try:
        saved = json.loads(path.read_text())
        policy = saved.get("policy")
        if policy not in POLICIES:
            raise ValueError("invalid advisor policy")
        ceiling = saved.get("ceiling") if policy == "auto" else None
        if ceiling is not None:
            validate_ceiling(ceiling)
    except (OSError, ValueError, AttributeError):
        policy = None
        ceiling = None
    try:
        squad = validate_squad(saved.get("squad"))
    except (ValueError, AttributeError):
        squad = {"enabled": False, "reviewer": None}
    return {
        "policy": policy or "always",
        "needs_policy_selection": policy is None or (policy == "auto" and ceiling is None),
        "ceiling": ceiling,
        "squad": squad,
        "policies": list(POLICIES),
        "scope": "model recommendations only; not execution permissions",
    }


def validate_catalog(models):
    """Accept host-verified models, not a hard-coded provider catalog."""
    if not isinstance(models, list) or len(models) > 100:
        raise ValueError("models must be a list of at most 100 host-verified models")
    seen = set()
    for model in models:
        if not isinstance(model, dict):
            raise ValueError("each model must be an object")
        model_id = model.get("id")
        efforts = model.get("reasoning_efforts")
        if not isinstance(model_id, str) or not model_id or model_id in seen:
            raise ValueError("models require unique nonempty ids")
        if not isinstance(efforts, list) or not efforts or any(
            not isinstance(effort, str) or not effort for effort in efforts
        ):
            raise ValueError("reasoning_efforts must list supported effort strings")
        if model.get("tier") not in (None, "fast", "balanced", "strong"):
            raise ValueError("optional tier must be fast, balanced or strong")
        seen.add(model_id)
    return models


def build_advice(result, models, current_model, settings):
    models = validate_catalog(models)
    answers = result.get("answers", {}) if isinstance(result, dict) else {}
    if not isinstance(answers, dict):
        answers = {}
    assessment = {}
    uncertain = False
    for key, question in ADVISOR_QUESTIONS.items():
        answer = answers.get(key, {})
        if not isinstance(answer, dict):
            answer = {}
        choice = answer.get("choice")
        confidence = answer.get("confidence")
        valid_confidence = (
            type(confidence) in (int, float) and math.isfinite(confidence)
            and CONFIDENCE_THRESHOLD <= confidence <= 1
        )
        if not isinstance(choice, str) or choice not in question["criteria"] or not valid_confidence:
            uncertain = True
        assessment[key] = {"choice": choice, "confidence": confidence}
    uncertain = uncertain or assessment["certainty"]["choice"] != "clear"
    high = any(assessment[key]["choice"] == "high" for key in ("complexity", "risk"))
    tier = "strong" if high or uncertain else (
        "fast" if all(assessment[key]["choice"] == "low" for key in ("complexity", "risk"))
        else "balanced"
    )
    # Only use explicit host/user tier mappings. Model names are not rankings.
    selected = next((model for model in models if model.get("tier") == tier), None)
    if selected is None:
        selected = next((model for model in models if model["id"] == current_model), None)
    ceiling = settings.get("ceiling") if settings["policy"] == "auto" else None
    ceiling_valid = False
    if settings["policy"] == "auto":
        # Models have no universal strength ordering: auto stays on the approved model.
        selected = None
        if isinstance(ceiling, dict):
            candidate = next((model for model in models if model["id"] == ceiling.get("model")), None)
            if candidate and ceiling.get("reasoning_effort") in candidate["reasoning_efforts"]:
                ceiling_valid = True
                selected = dict(candidate)
                index = candidate["reasoning_efforts"].index(ceiling["reasoning_effort"])
                selected["reasoning_efforts"] = candidate["reasoning_efforts"][:index + 1]
    recommendation = None
    if selected is not None:
        efforts = selected["reasoning_efforts"]
        preferred = {"fast": "low", "balanced": "medium", "strong": "high"}[tier]
        # The host supplies effort order from least to most reasoning.
        effort = preferred if preferred in efforts else efforts[
            {"fast": 0, "balanced": len(efforts) // 2, "strong": len(efforts) - 1}[tier]
        ]
        recommendation = {"model": selected["id"], "reasoning_effort": effort}
    policy = settings["policy"]
    needs_selection = settings["needs_policy_selection"] or (policy == "auto" and not ceiling_valid)
    ask = (needs_selection or policy == "always"
           or (policy == "conditional" and (high or uncertain)))
    return {
        "assessment": assessment,
        "uncertain": uncertain,
        "confidence_threshold": CONFIDENCE_THRESHOLD,
        "recommended_tier": tier,
        "recommendation": recommendation,
        "models": models,
        "policy": policy,
        "needs_policy_selection": needs_selection,
        "ceiling": ceiling,
        "ask_user": ask,
        "recommendation_accepted": not ask and recommendation is not None,
        "status": "awaiting_user" if ask else (
            "recommendation_accepted" if recommendation else "no_verified_recommendation"
        ),
        "model_switched": False,
        "requires_manual_switch": recommendation is not None,
    }
