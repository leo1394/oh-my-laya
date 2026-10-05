"""Whole-case packing against the checkpoint's actual tokenizer and packer."""
import hashlib
import json


POLICY_VERSION = "whole-case-token-budget-v1"


def pack_memory(agent, state, questions, cases, warning):
    cfg = getattr(agent, "cfg", None)
    if not isinstance(cfg, dict) or type(cfg.get("max_len")) is not int:
        raise ValueError("memory budget requires a verified checkpoint input window")
    maximum = cfg["max_len"]
    tok = getattr(agent, "tok", None)
    if not callable(tok) or not isinstance(getattr(tok, "mask_token", None), str):
        raise ValueError("memory budget requires the checkpoint tokenizer")

    def serialized(value):
        value = value if isinstance(value, str) else json.dumps(value, ensure_ascii=False)
        return value.replace(tok.mask_token, " ")

    def tokens(value):
        ids = tok(serialized(value), add_special_tokens=False)["input_ids"]
        if not isinstance(ids, list) or any(type(item) is not int for item in ids):
            raise ValueError("checkpoint tokenizer returned invalid token IDs")
        return ids

    empty, _ = agent.prepare("", questions)
    if (tokens("") or not isinstance(empty, list) or len(empty) != len(questions)
            or not empty or any(not isinstance(item.get("ids"), list)
                                or not item["ids"] for item in empty)):
        raise ValueError("checkpoint packing layout cannot be verified")
    room = min(maximum - len(item["ids"]) for item in empty)
    base_tokens = len(tokens(state))
    if base_tokens > room:
        raise ValueError("task exceeds checkpoint input window; summarize while preserving constraints before retrying")

    admitted = list(cases[:2])
    excluded = [{"case_id": case["id"], "reason": "case_limit", "required_state_tokens": None} for case in cases[2:]]
    while True:
        packed = {"current_state": state, "historical_case_context": {
            "warning": warning, "cases": admitted,
        }} if admitted else state
        state_ids = tokens(packed)
        if len(state_ids) <= room:
            break
        removed = admitted.pop()
        excluded.append({"case_id": removed["id"], "reason": "token_budget",
                         "required_state_tokens": len(state_ids)})

    # Verify the full real packing path, including all question prefixes and SEP.
    prepared, _ = agent.prepare(packed, questions)
    if len(prepared) != len(empty):
        raise ValueError("checkpoint packing changed while budgeting cases")
    for original, actual in zip(empty, prepared):
        expected = original["ids"][:-1] + state_ids + original["ids"][-1:]
        if actual["ids"] != expected or len(actual["ids"]) > maximum:
            raise ValueError("checkpoint packing would truncate or alter budgeted input")
    fingerprint = hashlib.sha256(json.dumps(
        [POLICY_VERSION, [item["ids"] for item in prepared]], separators=(",", ":")
    ).encode()).hexdigest()
    return packed, {
        "contract": "memory_receipt_v1", "policy_version": POLICY_VERSION,
        "selected_case_ids": [case["id"] for case in cases],
        "received_case_ids": [case["id"] for case in admitted], "excluded": excluded,
        "unit": "checkpoint_tokens", "available_state_tokens": room,
        "base_state_tokens": base_tokens, "packed_state_tokens": len(state_ids),
        "serialization": "laya-state-mask-normalized-utf8",
        "base_state_bytes": len(serialized(state).encode("utf-8")),
        "packed_state_bytes": len(serialized(packed).encode("utf-8")),
        "max_cases": 2, "complete_base_state": True, "input_fingerprint": fingerprint,
        "usage_scope": "current_evaluation",
    }
