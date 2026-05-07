#!/usr/bin/env python3
"""Emit mappings.yaml for the Proof catalog (Proof SDK HTTP surface). Run from repo root:
   python3 plasm-oss/apis/proof/_gen_mappings.py > plasm-oss/apis/proof/mappings.yaml
"""
from __future__ import annotations

import sys
from pathlib import Path

import yaml

# --- CML fragments (plain dicts → YAML) ---

LIT = lambda s: {"type": "literal", "value": s}
VAR = lambda n: {"type": "var", "name": n}


def if_var(name: str) -> dict:
    return {
        "type": "if",
        "condition": {"type": "exists", "var": name},
        "then_expr": {"type": "var", "name": name},
        "else_expr": {"type": "const", "value": None},
    }


def path_documents_slug() -> list:
    return [LIT("documents"), VAR("slug")]


def path_bridge_report_bug() -> list:
    """Hosted Proof accepts POST here (verified); POST /report/bug returns 404 on www.proofeditor.ai."""
    return [LIT("api"), LIT("bridge"), LIT("report_bug")]


def query_token_optional() -> dict:
    return {"type": "object", "fields": [["token", if_var("share_token")]]}


def headers_json() -> dict:
    return {
        "type": "object",
        "fields": [["Accept", {"type": "const", "value": "application/json"}]],
    }


def headers_agent() -> dict:
    return {"type": "object", "fields": [["X-Agent-Id", VAR("agent_id")]]}


def _host_idempotency_else(template: str, extra_vars: dict) -> dict:
    """Derive Idempotency-Key from Plasm execute session + mutation material (CML `format`)."""
    vars_: dict = {
        "ph": VAR("plasm_execute_prompt_hash"),
        "sid": VAR("plasm_execute_session_id"),
        "slug": VAR("slug"),
    }
    vars_.update(extra_vars)
    return {
        "type": "if",
        "condition": {"type": "exists", "var": "plasm_execute_prompt_hash"},
        "then_expr": {"type": "format", "template": template, "vars": vars_},
        "else_expr": {"type": "const", "value": None},
    }


def _idempotency_key_field(host_else: dict) -> dict:
    return {
        "type": "if",
        "condition": {"type": "exists", "var": "idempotency_key"},
        "then_expr": VAR("idempotency_key"),
        "else_expr": host_else,
    }


def headers_agent_mutations(host_else: dict) -> dict:
    """`X-Agent-Id` + Idempotency-Key (explicit or host-derived when `plasm_execute_*` env is set)."""
    return merge_header_objects(
        headers_agent(),
        {"type": "object", "fields": [["Idempotency-Key", _idempotency_key_field(host_else)]]},
    )


def merge_header_objects(a: dict, b: dict) -> dict:
    """Merge two CML object expressions (fields concatenated)."""
    af = a["fields"]
    bf = b["fields"]
    return {"type": "object", "fields": af + bf}


def headers_proof_sdk_bridge(agent_headers: dict) -> dict:
    """`/documents/.../bridge/*` routes require Proof SDK client handshake headers (see live 426 otherwise)."""
    return merge_header_objects(
        agent_headers,
        {
            "type": "object",
            "fields": [
                ["X-Proof-Client-Version", {"type": "const", "value": "0.30.0"}],
                ["X-Proof-Client-Protocol", {"type": "const", "value": "3"}],
                ["X-Proof-Client-Build", {"type": "const", "value": "plasm-agent"}],
            ],
        },
    )


def response_single() -> dict:
    return {"single": True}


def response_blocks() -> dict:
    return {"items_path": ["blocks"]}


def response_events() -> dict:
    # Pending events payload shape varies; common keys tried in SDK docs
    return {"items_path": ["events"]}


m: dict = {}

# --- Reads (share-style GET /d/:slug per proof-sdk agent docs; bearer auth via domain env) ---
m["document_get"] = {
    "method": "GET",
    "path": [LIT("d"), VAR("slug")],
    "query": query_token_optional(),
    "headers": headers_json(),
    "response": response_single(),
}

# Same route as document_get; Accept application/json so the HTTP layer decodes a JSON object
# (Plasm assumes table-shaped responses—raw text/markdown bodies are not decoded as rows).
m["document_get_markdown"] = {
    "method": "GET",
    "path": [LIT("d"), VAR("slug")],
    "query": query_token_optional(),
    "headers": headers_json(),
    "response": response_single(),
}

# Fallback mapping (agent HTTP/MCP intercepts this capability — no outbound HTTP on success).
m["document_share_bind"] = {
    "method": "GET",
    "path": [LIT("d"), VAR("slug")],
    "query": query_token_optional(),
    "headers": headers_json(),
    "response": response_single(),
}

m["editor_state_get"] = {
    "method": "GET",
    "path": path_documents_slug() + [LIT("state")],
    "query": {
        "type": "object",
        "fields": [
            ["kinds", if_var("kinds")],
            ["token", if_var("share_token")],
        ],
    },
    "headers": headers_json(),
    "response": response_single(),
}

m["block_query"] = {
    "method": "GET",
    "path": [LIT("documents"), VAR("document_id"), LIT("snapshot")],
    "query": query_token_optional(),
    "headers": headers_json(),
    "response": response_blocks(),
}

m["collaboration_event_query"] = {
    "method": "GET",
    # Scoped query parameter is `document_id` (entity ref → wire slug), same as `block_query`.
    "path": [LIT("documents"), VAR("document_id"), LIT("events"), LIT("pending")],
    "query": {
        "type": "object",
        "fields": [
            ["after", if_var("after")],
            ["limit", if_var("limit")],
            ["token", if_var("share_token")],
        ],
    },
    "headers": headers_json(),
    "response": response_events(),
}

# --- edit v2 (typed CGS input → JSON body; see `document_edit_v2` in domain.yaml) ---
m["document_edit_v2"] = {
    "method": "POST",
    "path": path_documents_slug() + [LIT("edit"), LIT("v2")],
    "query": query_token_optional(),
    "headers": headers_agent_mutations(
        _host_idempotency_else(
            "plasm:{ph}:{sid}:{slug}:edit_v2:{bt}",
            {"bt": VAR("base_token")},
        ),
    ),
    "body": {
        "type": "object",
        "fields": [
            ["by", VAR("by")],
            ["baseToken", VAR("base_token")],
            ["operations", VAR("operations")],
        ],
    },
    "response": response_single(),
}

# --- Bridge annotations ---
m["annotation_comment_add"] = {
    "method": "POST",
    "path": path_documents_slug() + [LIT("bridge"), LIT("comments")],
    "query": query_token_optional(),
    "headers": headers_proof_sdk_bridge(headers_agent()),
    "body": {
        "type": "object",
        "fields": [
            ["by", VAR("by")],
            ["quote", VAR("quote")],
            ["text", VAR("text")],
        ],
    },
    "response": response_single(),
}

m["annotation_comment_reply"] = {
    "method": "POST",
    "path": path_documents_slug() + [LIT("bridge"), LIT("comments"), LIT("reply")],
    "query": query_token_optional(),
    "headers": headers_proof_sdk_bridge(headers_agent()),
    "body": {
        "type": "object",
        "fields": [
            ["markId", VAR("mark_id")],
            ["by", VAR("by")],
            ["text", VAR("text")],
            [
                "resolve",
                {
                    "type": "if",
                    "condition": {"type": "exists", "var": "resolve"},
                    "then_expr": {"type": "var", "name": "resolve"},
                    "else_expr": {"type": "const", "value": None},
                },
            ],
        ],
    },
    "response": response_single(),
}

m["annotation_comment_resolve"] = {
    "method": "POST",
    "path": path_documents_slug() + [LIT("bridge"), LIT("comments"), LIT("resolve")],
    "query": query_token_optional(),
    "headers": headers_proof_sdk_bridge(headers_agent()),
    "body": {"type": "object", "fields": [["markId", VAR("mark_id")]]},
    "response": response_single(),
}

m["annotation_comment_unresolve"] = {
    "method": "POST",
    "path": path_documents_slug() + [LIT("ops")],
    "query": query_token_optional(),
    "headers": headers_agent(),
    "body": {
        "type": "object",
        "fields": [
            ["type", {"type": "const", "value": "comment.unresolve"}],
            ["by", VAR("by")],
            ["markId", VAR("mark_id")],
        ],
    },
    "response": response_single(),
}

m["annotation_suggestion_add"] = {
    "method": "POST",
    "path": path_documents_slug() + [LIT("bridge"), LIT("suggestions")],
    "query": query_token_optional(),
    "headers": headers_proof_sdk_bridge(headers_agent()),
    "body": {
        "type": "object",
        "fields": [
            ["by", VAR("by")],
            ["kind", VAR("suggestion_kind")],
            ["quote", VAR("quote")],
            ["content", if_var("content")],
            ["status", if_var("status")],
        ],
    },
    "response": response_single(),
}

m["annotation_suggestion_accept"] = {
    "method": "POST",
    "path": path_documents_slug() + [LIT("bridge"), LIT("marks"), LIT("accept")],
    "query": query_token_optional(),
    "headers": headers_proof_sdk_bridge(headers_agent()),
    "body": {"type": "object", "fields": [["markId", VAR("mark_id")]]},
    "response": response_single(),
}

m["annotation_suggestion_reject"] = {
    "method": "POST",
    "path": path_documents_slug() + [LIT("bridge"), LIT("marks"), LIT("reject")],
    "query": query_token_optional(),
    "headers": headers_proof_sdk_bridge(headers_agent()),
    "body": {"type": "object", "fields": [["markId", VAR("mark_id")]]},
    "response": response_single(),
}

m["annotation_comment_batch_apply"] = {
    "method": "POST",
    "path": path_documents_slug() + [LIT("ops")],
    "query": query_token_optional(),
    "headers": headers_agent(),
    "body": {
        "type": "object",
        "fields": [
            ["type", {"type": "const", "value": "comment.batch"}],
            ["by", VAR("by")],
            ["operations", VAR("operations")],
        ],
    },
    "response": response_single(),
}

m["document_title_update"] = {
    "method": "PUT",
    "path": path_documents_slug() + [LIT("title")],
    "query": query_token_optional(),
    "headers": headers_json(),
    "body": {"type": "object", "fields": [["title", VAR("title")]]},
    "response": response_single(),
}

m["collaboration_event_ack"] = {
    "method": "POST",
    "path": path_documents_slug() + [LIT("events"), LIT("ack")],
    "query": query_token_optional(),
    "headers": merge_header_objects(headers_agent(), headers_json()),
    "body": {
        "type": "object",
        "fields": [
            ["upToId", VAR("up_to_id")],
            ["by", VAR("by")],
        ],
    },
    "response": response_single(),
}

presence_status_expr = {
    "type": "if",
    "condition": {"type": "exists", "var": "presence_status"},
    "then_expr": {"type": "var", "name": "presence_status"},
    "else_expr": {"type": "const", "value": "online"},
}

m["presence_update"] = {
    "method": "POST",
    # Canonical agent presence (collab UI). Bridge route `.../bridge/presence` is for desktop/SDK
    # bridge clients — hosted editors treat `POST /documents/:slug/presence` (alias `/api/agent/:slug/presence`)
    # as the join signal.
    "path": path_documents_slug() + [LIT("presence")],
    "query": query_token_optional(),
    "headers": merge_header_objects(headers_agent(), headers_json()),
    "body": {"type": "object", "fields": [["status", presence_status_expr]]},
    "response": response_single(),
}

m["share_link_create"] = {
    "method": "POST",
    "path": [LIT("documents")],
    "body": {"type": "object", "fields": [["markdown", VAR("markdown")]]},
    "response": response_single(),
}

m["bug_report_submit"] = {
    "method": "POST",
    "path": path_bridge_report_bug(),
    "headers": headers_json(),
    "body": {"type": "object", "fields": []},
    "response": response_single(),
}

m["document_bug_report_submit"] = {
    "method": "POST",
    "path": path_bridge_report_bug(),
    "headers": headers_json(),
    "query": query_token_optional(),
    "body": {
        "type": "object",
        "fields": [
            ["slug", VAR("slug")],
        ],
    },
    "response": response_single(),
}


def main() -> None:
    out = Path(__file__).with_name("mappings.yaml")
    header = (
        "# Proof — HTTP mappings aligned with EveryInc/proof-sdk public routes.\n"
        "# See apis/proof/README.md for local curl + plasm-cli exploration.\n"
        "# Auth: domain `auth.env` bearer (PROOF_API_TOKEN); optional share `?token=` maps to `share_token`.\n\n"
    )
    # Stable key order
    text = header
    for k in sorted(m.keys()):
        text += yaml.dump({k: m[k]}, sort_keys=False, default_flow_style=False, allow_unicode=True)
    out.write_text(text, encoding="utf-8")
    print(f"wrote {out}", file=sys.stderr)


if __name__ == "__main__":
    main()
