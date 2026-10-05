"""Bounded raw assessments only; owned by the serialized worker process."""

from collections import OrderedDict
from copy import deepcopy
import hashlib
import json
import time


class AssessmentCache:
    def __init__(self, capacity=128, ttl=600, clock=time.monotonic):
        if capacity < 1 or ttl <= 0:
            raise ValueError("cache bounds must be positive")
        self.capacity = capacity
        self.ttl = ttl
        self.clock = clock
        self.entries = OrderedDict()

    def get(self, key):
        self.expire()
        entry = self.entries.get(key)
        if entry is None:
            return None
        self.entries.move_to_end(key)
        return deepcopy(entry[1])

    def put(self, key, value):
        self.expire()
        if len(json.dumps(value, ensure_ascii=False, allow_nan=False).encode("utf-8")) > 64 * 1024:
            return False
        self.entries[key] = (self.clock(), deepcopy(value))
        self.entries.move_to_end(key)
        while len(self.entries) > self.capacity:
            self.entries.popitem(last=False)
        return True

    def expire(self):
        now = self.clock()
        for key in [key for key, (created, _) in self.entries.items() if now - created >= self.ttl]:
            del self.entries[key]

    def clear(self):
        self.entries.clear()


def assessment_key(params, info, settings):
    context = params.get("_cache_context")
    orchestration = params.get("orchestration")
    if not isinstance(context, dict) or not isinstance(orchestration, dict) or orchestration.get("enabled") is not True:
        return None
    model = info.get("model", {})
    # Unknown checkpoint identity is never sufficient proof for reusing a result.
    if (not info.get("loaded") or not isinstance(model.get("checkpoint_digest"), str)
            or not model["checkpoint_digest"] or model.get("identity_scope") != "full_checkpoint_content"):
        return None
    snapshot = deepcopy(params)
    snapshot["advisor"].pop("role", None)
    snapshot["_cache_context"].pop("decision_id", None)
    value = {"contract": "assessment-reuse-v1", "params": snapshot, "worker": info, "settings": settings}
    encoded = json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False, allow_nan=False)
    return hashlib.sha256(encoded.encode("utf-8")).hexdigest()
