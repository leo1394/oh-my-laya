"""Opt-in, deterministic planning; never execution or recording authority."""

import hashlib
import json

from .routing import ROLES


POLICY_VERSION = "minimal-orchestration-v1"
CONTRACT = "orchestration_plan_v1"


def validate_context(context):
    fields = {"schema_version", "enabled", "run_id", "stage_id", "snapshot_revision",
              "independent_work", "dependencies_known", "required_roles", "constraint_refs"}
    if not isinstance(context, dict) or set(context) != fields:
        raise ValueError("orchestration requires the versioned context fields only")
    if type(context["schema_version"]) is not int or context["schema_version"] != 1:
        raise ValueError("unsupported orchestration schema_version")
    for field in ("enabled", "independent_work", "dependencies_known"):
        if type(context[field]) is not bool:
            raise ValueError(f"orchestration.{field} must be boolean")
    for field in ("run_id", "stage_id", "snapshot_revision"):
        value = context[field]
        if not isinstance(value, str) or not value.strip() or len(value) > 128:
            raise ValueError(f"orchestration.{field} requires a nonempty string up to 128 characters")
    for field, limit in (("required_roles", len(ROLES)), ("constraint_refs", 32)):
        value = context[field]
        if (not isinstance(value, list) or len(value) > limit
                or any(not isinstance(item, str) or not item.strip() or len(item) > 256 for item in value)
                or len(set(value)) != len(value)):
            raise ValueError(f"orchestration.{field} requires bounded unique strings")
    if any(role not in ROLES for role in context["required_roles"]):
        raise ValueError("orchestration.required_roles contains an unsupported role")
    return context


def build_plan(advice, context, state):
    """Consume existing validated advice, not a second set of model thresholds.

    Required roles are host-declared obligations. On needs_context they remain
    visible but are not dispatch instructions. The service binds decision_id.
    """
    validate_context(context)
    if not context["enabled"]:
        return None
    assessment = advice.get("assessment", {})
    complexity = assessment.get("complexity", {}).get("choice")
    risk = assessment.get("risk", {}).get("choice")
    certainty = assessment.get("certainty", {}).get("choice")
    roles = list(context["required_roles"])
    reasons = []
    if risk == "high" and "reviewer" not in roles:
        roles.append("reviewer")
        reasons.append("high_risk_requires_review")
    valid = complexity in ("low", "medium", "high") and risk in ("low", "medium", "high")
    if not valid or advice.get("uncertain") is not False or certainty != "clear":
        mode = "needs_context"
        reasons.append("uncertain_assessment")
    elif not context["dependencies_known"]:
        mode = "needs_context"
        reasons.append("unknown_dependencies")
    elif roles or context["independent_work"]:
        mode = "delegate"
        if context["independent_work"] and not roles:
            roles.append("worker")
        reasons.append("explicit_role_obligations" if context["required_roles"] else "bounded_work")
    else:
        mode = "direct"
        reasons.append("simple_local_task" if complexity == risk == "low" else "no_independent_work")
    encoded = json.dumps(state, sort_keys=True, separators=(",", ":"), ensure_ascii=False, allow_nan=False)
    return {
        "contract": CONTRACT,
        "schema_version": 1,
        "policy_version": POLICY_VERSION,
        "run_id": context["run_id"],
        "stage_id": context["stage_id"],
        "snapshot_revision": context["snapshot_revision"],
        "decision_id": None,
        "input_fingerprint": hashlib.sha256(encoded.encode("utf-8")).hexdigest(),
        "mode": mode,
        "reason_codes": reasons,
        "required_roles": roles,
        "constraint_refs": list(context["constraint_refs"]),
        "enforcement": "advisory",
        "execution_authorized": False,
        "parent_model_switched": False,
    }
