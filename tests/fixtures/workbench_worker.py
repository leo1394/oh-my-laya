#!/usr/bin/env python3
"""Deterministic process fixture; never used by production installation."""
import json
import sys
import time

for line in sys.stdin:
    request = json.loads(line)
    method = request["method"]
    if method == "info":
        result = {"model": {"model_revision": "fixture"}, "rules_version": "fixture"}
    elif method == "predict":
        if "advisor" not in request["params"]:
            assert "memory_cases" not in request["params"], "typed decisions must not receive advisor memory"
        if request["params"]["state"] == "fixture:slow":
            time.sleep(30)
        if request["params"]["state"] == "fixture:activity-slow":
            time.sleep(2)
        if request["params"]["state"] == "fixture:crash":
            sys.exit(2)
        answers = {key: {"choice": value, "confidence": 0.9} for key, value in
                   (("complexity", "low"), ("risk", "low"), ("certainty", "uncertain"))}
        result = {"laya_result": {"answers": answers},
                  "advice": {"uncertain": True, "ask_user": True, "assessment": answers},
                  "meta": {"case_ids": [case["id"] for case in request["params"].get("memory_cases", [])],
                           "routing_metadata_present": any(key in request["params"].get("advisor", {})
                                                           for key in ("task_family", "task_lineage"))}}
        if "advisor" not in request["params"]:
            result = {"answers": answers, "model": "fixture", "usage": {"input_tokens": 1, "output_tokens": 0}}
    else:
        result = {"policy": "always", "needs_policy_selection": True}
    print(json.dumps({"protocol_version": 1, "request_id": request["request_id"], "result": result}), flush=True)
