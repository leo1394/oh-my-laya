"""Private JSON Lines worker used by the local Laya service."""

import hashlib
import json
import os
from pathlib import Path
import sys

from . import server
from .advisor import ADVISOR_QUESTIONS
from .assessment_cache import AssessmentCache, assessment_key


PROTOCOL_VERSION = 1
MAX_INPUT_BYTES = 1024 * 1024
RULES_VERSION = "advisor-v1"
_CHECKPOINT_FILES = (
    "rl_agent_config.json",
    "encoder/config.json",
    "tokenizer/tokenizer_config.json",
    "tokenizer/tokenizer.json",
    "mlx_config.json",
)
_checkpoint_identity_cache = {}
_loaded_agent_id = None
_loaded_model = None
_assessment_cache = AssessmentCache()


class WorkerError(Exception):
    def __init__(self, code, message, details=None):
        super().__init__(message)
        self.code = code
        self.message = message
        self.details = details


def _stat_signature(path):
    stat = path.stat()
    return (stat.st_dev, stat.st_ino, stat.st_size, stat.st_mtime_ns, stat.st_ctime_ns)


def _checkpoint_identity(model_path):
    weight_path = model_path / "model.safetensors"
    try:
        paths = [weight_path]
        paths.extend(
            path for relative in _CHECKPOINT_FILES
            if (path := model_path / relative).is_file()
        )
        signature = tuple((str(path.relative_to(model_path)), _stat_signature(path)) for path in paths)
    except OSError:
        return None
    cache_key = str(model_path.resolve())
    cached = _checkpoint_identity_cache.get(cache_key)
    if cached is not None and cached[0] == signature:
        return cached[1]
    digest = hashlib.sha256()
    try:
        for path in paths:
            digest.update(str(path.relative_to(model_path)).encode("utf-8"))
            digest.update(b"\0")
            with path.open("rb") as stream:
                for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                    digest.update(chunk)
            digest.update(b"\0")
        final_signature = tuple(
            (str(path.relative_to(model_path)), _stat_signature(path)) for path in paths
        )
    except OSError:
        return None
    if final_signature != signature:
        return None
    checkpoint_digest = digest.hexdigest()
    _checkpoint_identity_cache[cache_key] = (signature, checkpoint_digest)
    return checkpoint_digest


def _configured_model_info(*, include_identity=True):
    configured_path = os.environ.get("LAYA_MODEL_DIR")
    model_path = Path(configured_path).expanduser() if configured_path else None
    checkpoint_digest = None
    if include_identity and model_path is not None and model_path.is_dir():
        checkpoint_digest = _checkpoint_identity(model_path)
    batch_size = os.environ.get("LAYA_BATCH_SIZE", "16")
    try:
        batch_size = int(batch_size)
    except ValueError:
        pass
    return {
        "path": str(model_path) if model_path is not None else None,
        "model_revision": checkpoint_digest,
        "checkpoint_digest": checkpoint_digest,
        "identity_scope": "full_checkpoint_content" if checkpoint_digest else None,
        "dtype": os.environ.get("LAYA_DTYPE", "float16"),
        "device": os.environ.get("LAYA_DEVICE", "gpu"),
        "batch_size": batch_size,
    }


def _unknown_identity(model, scope):
    model = dict(model)
    model["model_revision"] = None
    model["checkpoint_digest"] = None
    model["identity_scope"] = scope
    return model


def _bind_loaded_model(agent, before, after):
    global _loaded_agent_id, _loaded_model

    _loaded_agent_id = id(agent)
    if before != after:
        _loaded_model = _unknown_identity(
            before, "unknown_files_changed_during_load"
        )
    elif before["checkpoint_digest"] is not None:
        _loaded_model = dict(before)
    else:
        _loaded_model = _unknown_identity(
            before, "unknown_checkpoint_identity_at_load"
        )


def _model_info():
    loaded_agent = server._agent
    if loaded_agent is None:
        model = _configured_model_info()
    elif _loaded_agent_id == id(loaded_agent) and _loaded_model is not None:
        model = dict(_loaded_model)
    else:
        model = _unknown_identity(
            _configured_model_info(include_identity=False),
            "unknown_loaded_checkpoint",
        )
    return {
        "loaded": loaded_agent is not None,
        "model": model,
        "advisor_questions": ADVISOR_QUESTIONS,
        "rules_version": RULES_VERSION,
        "supported_contracts": ["orchestration_plan_v1", "assessment_reuse_v1", "memory_budget_v1"],
    }


def _predict(params):
    allowed = {"state", "questions", "advisor", "memory_cases", "model_tiers", "orchestration", "_cache_context", "memory_budget"}
    unknown = set(params) - allowed
    if unknown:
        raise ValueError("predict contains unknown parameters")
    if "state" not in params:
        raise ValueError("predict requires state")
    if type(params.get("memory_budget", False)) is not bool:
        raise ValueError("memory_budget must be a boolean")
    if "memory_cases" in params and params["memory_cases"] is None:
        raise ValueError("memory_cases must be a list of at most 3 cases")
    context = params.get("_cache_context")
    if context is not None:
        if (not isinstance(context, dict) or set(context) != {"decision_id", "service_instance", "epoch", "memory_version", "settings"}
                or not isinstance(context["decision_id"], str) or not context["decision_id"]
                or not isinstance(context["service_instance"], str) or not context["service_instance"]
                or type(context["epoch"]) is not int or context["epoch"] < 0
                or not isinstance(params.get("advisor"), dict)):
            raise ValueError("invalid private assessment cache context")
    settings = server.preferences() if context is not None else None
    key = assessment_key(params, _model_info(), settings) if context is not None else None
    cached = _assessment_cache.get(key) if key is not None else None
    loading = server._agent is None
    before = _configured_model_info() if loading else None
    arguments = {
        "memory_cases": params.get("memory_cases", [] if params.get("advisor") is not None else None),
        "model_tiers": params.get("model_tiers"), "orchestration": params.get("orchestration"),
        "memory_budget": params.get("memory_budget", False),
    }
    try:
        result = server.run_prediction(
            params["state"],
            params.get("questions"),
            params.get("advisor"),
            **arguments,
            _cached_result=cached["raw"] if cached else server._NO_CACHED_RESULT,
        )
        fingerprint = result.get("meta", {}).get("memory_receipt", {}).get("input_fingerprint")
        if cached is not None and fingerprint != cached.get("input_fingerprint"):
            cached = None
            result = server.run_prediction(params["state"], params.get("questions"),
                                           params.get("advisor"), **arguments)
    finally:
        if loading and server._agent is not None:
            after = _configured_model_info()
            _bind_loaded_model(server._agent, before, after)
    receipt = result.get("meta", {}).get("memory_receipt")
    if receipt is not None:
        receipt["checkpoint_identity"] = _model_info()["model"]
    if context is not None:
        meta = result.setdefault("meta", {})
        if cached is not None:
            if receipt is not None:
                receipt["usage_scope"] = "source_evaluation"
                receipt["reused_from_decision_id"] = cached["decision_id"]
            meta["reused_from_decision_id"] = cached["decision_id"]
            meta["assessment_cache"] = {"status": "hit", "inference_input_tokens": 0,
                                        "inference_output_tokens": 0, "raw_usage_scope": "source_evaluation"}
            if "orchestration_plan" in result:
                result["orchestration_plan"]["reused_from_decision_id"] = cached["decision_id"]
        else:
            key = assessment_key(params, _model_info(), settings)
            raw = result.get("laya_result")
            if (key is not None and isinstance(raw, dict) and isinstance(raw.get("answers"), dict)
                    and settings == server.preferences()):
                _assessment_cache.put(key, {"raw": raw, "decision_id": context["decision_id"],
                                            "input_fingerprint": receipt.get("input_fingerprint") if receipt else None})
            meta["assessment_cache"] = {"status": "miss" if key is not None else "unavailable",
                                        "raw_usage_scope": "current_evaluation"}
    return result


def _preferences(params):
    allowed = {"policy", "ceiling", "models", "squad"}
    unknown = set(params) - allowed
    if unknown:
        raise ValueError("preferences contains unknown parameters")
    result = server.laya_advisor_preferences(
        params.get("policy"), params.get("ceiling"), params.get("models"),
        params.get("squad"),
    )
    if any(params.get(key) is not None for key in ("policy", "ceiling", "squad")):
        _assessment_cache.clear()
    return result


def _dispatch(method, params):
    if method == "clear_assessment_cache":
        if params:
            raise ValueError("clear_assessment_cache does not accept parameters")
        _assessment_cache.clear()
        return {"cleared": True}, False
    if method == "predict":
        return _predict(params), False
    if method == "preferences":
        return _preferences(params), False
    if method == "info":
        if params:
            raise ValueError("info does not accept parameters")
        return _model_info(), False
    if method == "shutdown":
        if params:
            raise ValueError("shutdown does not accept parameters")
        return {"shutdown": True}, True
    raise WorkerError("method_not_found", "unknown method")


def _handle(payload):
    if not isinstance(payload, dict):
        raise WorkerError("invalid_request", "request must be a JSON object")
    if set(payload) != {"protocol_version", "request_id", "method", "params"}:
        raise WorkerError(
            "invalid_request",
            "request requires only protocol_version, request_id, method and params",
        )
    if payload["protocol_version"] != PROTOCOL_VERSION:
        raise WorkerError("unsupported_protocol", "protocol_version must be 1")
    if not isinstance(payload["request_id"], str) or not payload["request_id"]:
        raise WorkerError("invalid_request", "request_id must be a nonempty string")
    if not isinstance(payload["method"], str) or not isinstance(payload["params"], dict):
        raise WorkerError("invalid_request", "method must be a string and params an object")
    return _dispatch(payload["method"], payload["params"])


def _response(request_id, *, result=None, error=None):
    response = {
        "protocol_version": PROTOCOL_VERSION,
        "request_id": request_id,
    }
    if error is None:
        response["result"] = result
    else:
        response["error"] = error
    return response


def _write(stream, response):
    try:
        output = json.dumps(
            response, ensure_ascii=False, allow_nan=False, separators=(",", ":")
        ).encode("utf-8")
    except (TypeError, ValueError):
        output = json.dumps(_response(
            response.get("request_id"),
            error={"code": "serialization_error", "message": "result is not valid JSON"},
        ), separators=(",", ":")).encode("utf-8")
    stream.write(output + b"\n")
    stream.flush()


def _discard_line(stream):
    while True:
        chunk = stream.readline(MAX_INPUT_BYTES + 1)
        if not chunk or chunk.endswith(b"\n"):
            return


def _reject_json_constant(_value):
    raise WorkerError("invalid_json", "request is not valid UTF-8 JSON")


def serve(stdin, stdout, stderr):
    while True:
        line = stdin.readline(MAX_INPUT_BYTES + 2)
        if not line:
            return
        if len(line) > MAX_INPUT_BYTES + 1 or (
            len(line) == MAX_INPUT_BYTES + 1 and not line.endswith(b"\n")
        ):
            if not line.endswith(b"\n"):
                _discard_line(stdin)
            _write(stdout, _response(
                None,
                error={"code": "request_too_large", "message": "request exceeds 1 MiB"},
            ))
            continue
        encoded = line[:-1] if line.endswith(b"\n") else line
        if len(encoded) > MAX_INPUT_BYTES:
            _write(stdout, _response(
                None,
                error={"code": "request_too_large", "message": "request exceeds 1 MiB"},
            ))
            continue
        request_id = None
        stop = False
        try:
            payload = json.loads(
                encoded.decode("utf-8"),
                parse_constant=_reject_json_constant,
            )
            if isinstance(payload, dict):
                candidate = payload.get("request_id")
                if isinstance(candidate, str):
                    request_id = candidate
            result, stop = _handle(payload)
            response = _response(request_id, result=result)
        except (UnicodeDecodeError, json.JSONDecodeError):
            response = _response(
                request_id,
                error={"code": "invalid_json", "message": "request is not valid UTF-8 JSON"},
            )
        except WorkerError as error:
            payload = {"code": error.code, "message": error.message}
            if error.details is not None:
                payload["details"] = error.details
            response = _response(
                request_id, error=payload
            )
        except server.PredictionFailure as error:
            print(f"model prediction failed: {error.exception_type}", file=stderr)
            response = _response(
                request_id,
                error={
                    "code": "prediction_error",
                    "message": "model prediction failed",
                    "details": {
                        "phase": "prediction",
                        "exception_type": error.exception_type,
                    },
                },
            )
        except server.ModelLoadFailure as error:
            print(f"model load failed: {error.exception_type}", file=stderr)
            response = _response(
                request_id,
                error={
                    "code": "model_load_error",
                    "message": "model load failed",
                    "details": {
                        "phase": "model_load",
                        "exception_type": error.exception_type,
                    },
                },
            )
        except ValueError as error:
            response = _response(
                request_id, error={"code": "invalid_params", "message": str(error)}
            )
        except Exception as error:
            print(f"worker request failed: {type(error).__name__}", file=stderr)
            response = _response(
                request_id,
                error={
                    "code": "internal_error",
                    "message": "worker request failed",
                    "details": {
                        "phase": "dispatch",
                        "exception_type": type(error).__name__,
                    },
                },
            )
        _write(stdout, response)
        if stop:
            return


def main():
    inherited_lock = os.environ.pop("LAYA_MODEL_LOCK_FD", None)
    if inherited_lock is not None:
        # Keep the lock in this model process, not in future exec'd descendants.
        os.set_inheritable(int(inherited_lock), False)
    serve(sys.stdin.buffer, sys.stdout.buffer, sys.stderr)


if __name__ == "__main__":
    main()
