"""Structured agent actions, strict JSON parsing, and explicit error types."""

from __future__ import annotations

import json


NO_VCS = (
    "Do not commit, push, stage, unstage, stash, or revert anything. "
    "Leave every change in the working tree for human review."
)


def _object_schema(properties: dict) -> dict:
    return {
        "type": "object", "additionalProperties": False,
        "required": list(properties), "properties": properties,
    }


STRING = {"type": "string"}

STRINGS = {"type": "array", "items": STRING}

PROPOSAL_SCHEMA = _object_schema({
    "action": {"type": "string", "enum": ["PATCH", "COMPLETE"]},
    "patch": STRING,
    "explanation": STRING,
    "context": STRING,
    "context_paths": STRINGS,
    "completion_evidence": {"type": "array", "items": _object_schema({
        "criterion": STRING, "evidence": STRING,
    })},
})

PATCH_REVIEW_SCHEMA = _object_schema({
    "decision": {"type": "string", "enum": ["APPROVED", "REVISED", "NEEDS_CONTEXT"]},
    "patch": STRING,
    "explanation": STRING,
    "context_requests": STRINGS,
})

AUTHOR_SCHEMA = _object_schema({
    "action": {"type": "string", "enum": ["CONTINUE", "COMPLETE"]},
    "explanation": STRING,
    "completion_evidence": {"type": "array", "items": _object_schema({
        "criterion": STRING, "evidence": STRING,
    })},
})

REVIEW_SCHEMA = _object_schema({
    "decision": {"type": "string", "enum": ["APPROVED", "CHANGES_REQUIRED"]},
    "explanation": STRING,
    "findings": {"type": "array", "items": _object_schema({
        "path": STRING, "problem": STRING,
    })},
})


def _schema_error(value, schema: dict, path: str = "$" ) -> str | None:
    expected = schema.get("type")
    types = {
        "object": dict,
        "array": list,
        "string": str,
        "boolean": bool,
        "number": (int, float),
        "integer": int,
    }
    if expected in types and (not isinstance(value, types[expected]) or expected in {"number", "integer"} and isinstance(value, bool)):
        return f"{path} must be {expected}"
    if "enum" in schema and value not in schema["enum"]:
        return f"{path} is not an allowed value"
    if expected == "object":
        required = schema.get("required", [])
        missing = [key for key in required if key not in value]
        if missing:
            return f"{path} missing required fields: {', '.join(missing)}"
        properties = schema.get("properties", {})
        if schema.get("additionalProperties") is False:
            extra = sorted(set(value) - set(properties))
            if extra:
                return f"{path} has additional fields: {', '.join(extra)}"
        for key, child in properties.items():
            if key in value:
                error = _schema_error(value[key], child, f"{path}.{key}")
                if error:
                    return error
    if expected == "array":
        child = schema.get("items", {})
        for index, item in enumerate(value):
            error = _schema_error(item, child, f"{path}[{index}]")
            if error:
                return error
    return None


class CrewError(RuntimeError):
    """An explicit stop; no automatic rebasing, fallback writer, or rollback."""


class ProtocolError(CrewError):
    """A failed invocation/response that may be retried without writing source."""


def load_json(raw: str):
    def unique_object(pairs):
        value = {}
        for key, item in pairs:
            if key in value:
                raise ValueError(f"duplicate JSON field: {key}")
            value[key] = item
        return value
    def reject_constant(value):
        raise ValueError(f"invalid JSON constant: {value}")
    value = json.loads(raw, object_pairs_hook=unique_object, parse_constant=reject_constant)
    json.dumps(value, ensure_ascii=False).encode('utf-8')  # reject lone surrogates
    return value


def parse_response(raw: str, schema: dict) -> dict:
    try:
        value = load_json(raw)
    except (ValueError, TypeError) as exc:
        raise ProtocolError(f"malformed JSON response: {exc}") from exc
    error = _schema_error(value, schema)
    if error:
        raise ProtocolError(error)
    return value


def parse_proposal(raw: str) -> dict:
    value = parse_response(raw, PROPOSAL_SCHEMA)
    if not value["explanation"].strip():
        raise ProtocolError("proposal needs an explanation")
    if value["action"] == "PATCH":
        if not value["patch"].strip() or value["completion_evidence"]:
            raise ProtocolError("PATCH needs a complete diff and empty completion_evidence")
    elif value["patch"] or not value["completion_evidence"] or any(
        not entry["criterion"].strip() or not entry["evidence"].strip()
        for entry in value["completion_evidence"]
    ):
        raise ProtocolError("COMPLETE needs an empty patch and acceptance-criterion evidence")
    return value


def parse_author(raw: str) -> dict:
    value = parse_response(raw, AUTHOR_SCHEMA)
    if not value['explanation'].strip():
        raise ProtocolError('Author response needs an explanation')
    evidence = value['completion_evidence']
    if value['action'] == 'COMPLETE' and (not evidence or any(
        not item['criterion'].strip() or not item['evidence'].strip() for item in evidence
    )):
        raise ProtocolError('COMPLETE requires acceptance-criterion evidence')
    return value


def parse_review(raw: str) -> dict:
    value = parse_response(raw, REVIEW_SCHEMA)
    if not value['explanation'].strip():
        raise ProtocolError('Review requires an explanation')
    if value['decision'] == 'APPROVED' and value['findings']:
        raise ProtocolError('APPROVED cannot have unresolved findings')
    if value['decision'] == 'CHANGES_REQUIRED' and (not value['findings'] or any(
        not item['problem'].strip() for item in value['findings']
    )):
        raise ProtocolError('CHANGES_REQUIRED requires concrete findings')
    return value
