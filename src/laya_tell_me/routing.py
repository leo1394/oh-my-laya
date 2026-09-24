"""Validated spawn recommendations, never a delegation or execution mechanism."""

from .advisor import build_advice, validate_ceiling, validate_squad


ROLES = ("explorer", "worker", "tester", "researcher", "reviewer")


def pair_supported(pair, models):
    try:
        validate_ceiling(pair)
    except ValueError:
        return False
    return any(
        model["id"] == pair["model"]
        and pair["reasoning_effort"] in model["reasoning_efforts"]
        for model in models
    )


def build_role_advice(result, models, current_model, current_effort, role, settings,
                      execution_choice=None):
    if role not in ROLES:
        raise ValueError(f"role must be one of {ROLES}; orchestrator is never routed")
    advice = build_advice(result, models, current_model, settings)
    parent = {"model": current_model, "reasoning_effort": current_effort}
    try:
        squad = validate_squad(settings.get("squad"))
    except ValueError:
        squad = {"enabled": False, "reviewer": None}
    reason = "advisor_policy"
    if role != "reviewer" and settings["policy"] != "auto":
        # Non-auto setup chooses a session-only pair, not a persisted ceiling.
        # Preserve that explicit choice instead of silently using the parent.
        advice["recommendation"] = execution_choice
        reason = "session_execution_choice"
    if role == "reviewer":
        difficult = advice["uncertain"] or any(
            advice["assessment"][key]["choice"] == "high"
            for key in ("complexity", "risk")
        )
        use_parent = difficult or squad["reviewer"] is None
        advice["recommendation"] = parent if use_parent else squad["reviewer"]
        reason = "orchestrator_for_difficult_review" if difficult else (
            "orchestrator_default" if use_parent else "configured_reviewer"
        )
    valid = pair_supported(parent, models) and pair_supported(advice["recommendation"], models)
    configured = squad["enabled"] and not advice["needs_policy_selection"]
    if not valid or not configured:
        advice["ask_user"] = True
    advice["recommendation_accepted"] = not advice["ask_user"] and valid and configured
    advice["status"] = "awaiting_user" if advice["ask_user"] else "recommendation_accepted"
    advice["requires_manual_switch"] = False
    advice["delegation"] = {
        "role": role,
        "reason": reason,
        "needs_setup": not configured,
        "needs_verified_pair": not valid,
        "assignment": advice["recommendation"] if valid else None,
        "spawn_parameters": {
            "agent_type": role,
            "model": advice["recommendation"]["model"],
            "reasoning_effort": advice["recommendation"]["reasoning_effort"],
            "fork_turns": "none",
        } if advice["recommendation_accepted"] else None,
        "parent_model_switched": False,
        "agent_spawned": False,
        "execution_authorized": False,
    }
    return advice
