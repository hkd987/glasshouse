"""A stand-in for api.anthropic.com's POST /v1/messages, for testing the
harness without credits (tools/dev/fake-model.sh runs it).

It checks requests the way the documented API does (claude-api reference,
read 2026-10-22): strict top-level fields, alternating roles, every tool_use
answered by a tool_result at the start of the next user turn, thinking
blocks bound to the conversation before them (system, tools as a
name-sorted set, earlier messages; cache_control markers ignored) and
chained to the previous thinking block. A replayed block whose prefix
changed is the documented 400. The real API is a little looser (it ignores
trailing whitespace); this errs strict. Replies are Anthropic shaped: an
empty thinking block (display "omitted") with a signature, then text and
tool_use blocks. Usage counts ~4 characters a token, the tools+system
breakpoint cached after its first write (>= 512 tokens).

Every request body is appended to --log as one JSON line, with the reply.

Scenarios (the model's behavior), by --scenario:
  desktop     list the folder, read each file, draft a mail to an address it
              found (a redacted placeholder), then answer
  parallel    the same, but reads every file in one turn (parallel tool_use)
  overload2   desktop, but the second call gets 529 overloaded_error once
  ratelimit2  desktop, but the second call gets 429 with retry-after once
  refusal2    desktop, but the second call is declined (stop_reason refusal)
  maxtokens1  the first step runs out of max_tokens while thinking
  maxtokens   every call does
  credit      every call is the out-of-credit 400 the real API sent us
  billing     every call is a 402 billing_error
  triage      for the IT agent: a JSON triage, then a short assessment
"""
import argparse
import hashlib
import hmac
import http.server
import json
import re
import ssl
import sys
import threading

SECRET = b"fake-anthropic-signing-key"
TOP_LEVEL = {"model", "max_tokens", "messages", "system", "tools", "cache_control", "thinking",
             "output_config", "tool_choice", "metadata", "stop_sequences", "stream", "service_tier",
             "context_management", "container", "mcp_servers", "inference_geo"}
NO_LONGER = {"temperature", "top_p", "top_k"}  # 400 on every model since 2026-10-21
USER_BLOCKS = {"text", "tool_result", "image", "document", "search_result"}
ASSISTANT_BLOCKS = {"text", "thinking", "redacted_thinking", "tool_use", "server_tool_use"}
TOOL_NAME = re.compile(r"^[a-zA-Z0-9_-]{1,128}$")

lock = threading.Lock()
state = {"calls": 0, "faults": set(), "cached": set()}


class Bad(Exception):
    def __init__(self, status, kind, message):
        super().__init__(message)
        self.status, self.kind, self.message = status, kind, message


def strip_cache(value):
    """The value with cache_control markers removed: they don't bind."""
    if isinstance(value, dict):
        return {k: strip_cache(v) for k, v in value.items() if k != "cache_control"}
    if isinstance(value, list):
        return [strip_cache(v) for v in value]
    return value


def blocks(content):
    return [{"type": "text", "text": content}] if isinstance(content, str) else content


def system_text(system):
    if system is None:
        return []
    if isinstance(system, str):
        return [system]
    return [b["text"] for b in system]


def canonical(value):
    return json.dumps(strip_cache(value), sort_keys=True, separators=(",", ":"), ensure_ascii=False)


def sign(body, before, previous):
    """A signature over the conversation before the block, chained to the
    previous thinking block's."""
    tools = sorted(body.get("tools") or [], key=lambda t: t.get("name", ""))
    prefix = canonical({"system": system_text(body.get("system")), "tools": tools,
                        "messages": [{"role": m["role"], "content": blocks(m["content"])} for m in before]})
    return hmac.new(SECRET, (previous + "\0" + prefix).encode(), hashlib.sha256).hexdigest()


def check(body, headers):
    if not headers.get("x-api-key"):
        raise Bad(401, "authentication_error", "x-api-key header is required")
    if headers.get("anthropic-version") != "2023-06-01":
        raise Bad(400, "invalid_request_error", "anthropic-version: unexpected value")
    for key in body:
        if key in NO_LONGER:
            raise Bad(400, "invalid_request_error", f"{key}: is not supported on this API version")
        if key not in TOP_LEVEL:
            raise Bad(400, "invalid_request_error", f"{key}: Extra inputs are not permitted")
    for key in ("model", "max_tokens", "messages"):
        if key not in body:
            raise Bad(400, "invalid_request_error", f"{key}: Field required")
    if not 1 <= body["max_tokens"] <= 128000:
        raise Bad(400, "invalid_request_error", "max_tokens: out of range")
    thinking = body.get("thinking") or {}
    if thinking.get("type") in ("disabled", "enabled"):
        raise Bad(400, "invalid_request_error",
                  f'"thinking.type.{thinking["type"]}" is not supported for this model.')
    # The history check is asked for under its beta; this stand-in always
    # holds blocks to their conversation, as "error" does.
    if "block_binding" in thinking:
        betas = [b.strip() for b in headers.get("anthropic-beta", "").split(",")]
        if "thinking-binding-controls-2026-08-01" not in betas:
            raise Bad(400, "invalid_request_error", "thinking.block_binding: Extra inputs are not permitted")
        if thinking["block_binding"].get("prefix_mismatch_behavior") not in ("error", "drop_block"):
            raise Bad(400, "invalid_request_error", "thinking.block_binding.prefix_mismatch_behavior: invalid value")
    if (body.get("tool_choice") or {}).get("type") in ("any", "tool"):
        raise Bad(400, "invalid_request_error", 'tool_choice: type "tool" and "any" are not supported for this model.')
    breakpoints = 1 if "cache_control" in body else 0
    for b in body.get("system") or [] if not isinstance(body.get("system"), str) else []:
        if b.get("type") != "text" or not isinstance(b.get("text"), str):
            raise Bad(400, "invalid_request_error", "system: blocks must be text")
        breakpoints += "cache_control" in b
    names = set()
    for i, t in enumerate(body.get("tools") or []):
        extra = set(t) - {"name", "description", "input_schema", "cache_control", "strict", "type",
                          "defer_loading", "eager_input_streaming", "input_examples"}
        if extra:
            raise Bad(400, "invalid_request_error", f"tools.{i}.{sorted(extra)[0]}: Extra inputs are not permitted")
        if not TOOL_NAME.match(t.get("name", "")) or t["name"] in names:
            raise Bad(400, "invalid_request_error", f"tools.{i}.name: invalid or repeated")
        names.add(t["name"])
        if (t.get("input_schema") or {}).get("type") != "object":
            raise Bad(400, "invalid_request_error", f"tools.{i}.input_schema.type: must be 'object'")
        breakpoints += "cache_control" in t
    msgs = body["messages"]
    if not msgs or msgs[0]["role"] != "user":
        raise Bad(400, "invalid_request_error", "messages: the first message must use the user role")
    pending = []
    previous = ""
    for i, m in enumerate(msgs):
        if set(m) - {"role", "content"}:
            raise Bad(400, "invalid_request_error", f"messages.{i}: Extra inputs are not permitted")
        if i and msgs[i - 1]["role"] == m["role"]:
            raise Bad(400, "invalid_request_error", 'messages: roles must alternate between "user" and "assistant"')
        content = blocks(m["content"])
        if isinstance(m["content"], str) and not m["content"].strip() and i < len(msgs) - 1:
            raise Bad(400, "invalid_request_error", f"messages.{i}: text content blocks must be non-empty")
        allowed = USER_BLOCKS if m["role"] == "user" else ASSISTANT_BLOCKS
        answered = []
        seen_other = False
        for j, b in enumerate(content):
            kind = b.get("type")
            if kind not in allowed:
                raise Bad(400, "invalid_request_error", f"messages.{i}.content.{j}.type: '{kind}' not allowed for role {m['role']}")
            breakpoints += "cache_control" in b
            if kind == "text":
                if set(b) - {"type", "text", "cache_control", "citations"} or not isinstance(b.get("text"), str):
                    raise Bad(400, "invalid_request_error", f"messages.{i}.content.{j}: bad text block")
                if not b["text"].strip():
                    raise Bad(400, "invalid_request_error", f"messages.{i}.content.{j}: text content blocks must be non-empty")
                seen_other = True
            elif kind == "tool_result":
                if set(b) - {"type", "tool_use_id", "content", "is_error", "cache_control"}:
                    raise Bad(400, "invalid_request_error", f"messages.{i}.content.{j}: Extra inputs are not permitted")
                if seen_other:
                    raise Bad(400, "invalid_request_error",
                              f"messages.{i}: tool_result blocks must come before any other content")
                if b.get("tool_use_id") not in pending:
                    raise Bad(400, "invalid_request_error",
                              f"messages.{i}.content.{j}: unexpected `tool_use_id` found in `tool_result` blocks: {b.get('tool_use_id')}. "
                              "Each `tool_result` block must have a corresponding `tool_use` block in the previous message.")
                answered.append(b["tool_use_id"])
            elif kind == "thinking":
                if set(b) != {"type", "thinking", "signature"}:
                    raise Bad(400, "invalid_request_error", f"messages.{i}.content.{j}: bad thinking block")
                expected = sign(body, msgs[:i], previous)
                if b["signature"] != expected:
                    raise Bad(400, "invalid_request_error",
                              f"messages.{i}.content.{j}: Invalid `signature` in `thinking` block. "
                              "The block is bound to a different conversation.")
                previous = b["signature"]
            elif kind == "tool_use":
                if set(b) - {"type", "id", "name", "input", "cache_control", "caller"} or not isinstance(b.get("input"), dict):
                    raise Bad(400, "invalid_request_error", f"messages.{i}.content.{j}: bad tool_use block")
                if b["name"] not in names:
                    raise Bad(400, "invalid_request_error", f"messages.{i}.content.{j}: tool {b['name']!r} is not in tools")
        if m["role"] == "user":
            missing = [p for p in pending if p not in answered]
            if missing:
                raise Bad(400, "invalid_request_error",
                          f"messages.{i}: `tool_use` ids were found without `tool_result` blocks immediately after: {missing}")
            pending = []
        else:
            pending = [b["id"] for b in content if b.get("type") == "tool_use"]
    if msgs[-1]["role"] != "user":
        raise Bad(400, "invalid_request_error", "This model does not support assistant message prefill.")
    if breakpoints > 4:
        raise Bad(400, "invalid_request_error", "A maximum of 4 blocks with cache_control may be provided.")
    return previous


def tokens(text):
    return max(1, len(text) // 4)


def usage(body, output):
    tools = sorted(body.get("tools") or [], key=lambda t: t["name"])
    head = canonical({"tools": tools, "system": system_text(body.get("system"))})
    total = tokens(canonical(body))
    marked = any("cache_control" in b for b in body.get("system") or [] if isinstance(b, dict))
    written = read = 0
    if marked and tokens(head) >= 512:
        key = hashlib.sha256((body["model"] + head).encode()).hexdigest()
        with lock:
            if key in state["cached"]:
                read = tokens(head)
            else:
                written = tokens(head)
                state["cached"].add(key)
    return {"input_tokens": total - written - read, "cache_creation_input_tokens": written,
            "cache_read_input_tokens": read, "output_tokens": output}


def tool(body, *hints):
    for t in body.get("tools") or []:
        if all(h in t["name"] for h in hints):
            return t["name"]
    raise Bad(400, "invalid_request_error", f"scenario: no tool like {hints}")


def results(msgs):
    """Every tool_result so far, by tool_use id, with the call that asked."""
    calls = {}
    out = []
    for m in msgs:
        for b in blocks(m["content"]):
            if b.get("type") == "tool_use":
                calls[b["id"]] = b
            elif b.get("type") == "tool_result":
                text = b["content"] if isinstance(b["content"], str) else "".join(x.get("text", "") for x in b["content"])
                out.append((calls[b["tool_use_id"]], text, b.get("is_error", False)))
    return out


def model(body, scenario, previous, n):
    """The scripted model: (content blocks after thinking, stop_reason)."""
    msgs = body["messages"]
    turn = sum(1 for m in msgs if m["role"] == "assistant")
    done = results(msgs)
    use = lambda name, inp, k=0: {"type": "tool_use", "id": f"toolu_{n:03d}_{k}", "name": name, "input": inp}
    if scenario == "triage":
        if "areas" in canonical(body.get("system")):
            return [{"type": "text", "text": '{"areas": ["performance"], "restart": null, "no_fix": false}'}], "end_turn"
        return [{"type": "text", "text": "Likely a runaway process; the checks found nothing to fix here."}], "end_turn"
    folder = re.search(r"my (\w+) folder", msgs[0]["content"] if isinstance(msgs[0]["content"], str) else "")
    folder = folder.group(1) if folder else "."
    lister, reader = tool(body, "files", "list"), tool(body, "files", "read")
    listed = [r for r in done if r[0]["name"] == lister]
    reads = [r for r in done if r[0]["name"] == reader]
    drafted = [r for r in done if "mail_draft" in r[0]["name"]]
    if not listed:
        return [{"type": "text", "text": "I'll look in the folder first."},
                use(lister, {"path": folder})], "tool_use"
    names = re.findall(r"[\w.-]+\.txt", listed[-1][1])
    unread = [f for f in names if not any(f in canonical(r[0]["input"]) for r in reads)]
    if unread:
        files = unread if scenario == "parallel" else unread[:1]
        return [use(reader, {"path": f"{folder}/{f}"}, k) for k, f in enumerate(files)], "tool_use"
    found = re.findall(r"\[redacted:email#\d+\]", " ".join(r[1] for r in reads))
    if found and not drafted:
        try:
            draft = tool(body, "mail", "draft")
        except Bad:
            draft = None
        if draft:
            return [use(draft, {"to": [found[0]], "subject": "Launch date",
                                "body": f"Hi, can we confirm the launch date? I'll write to {found[0]} again Thursday."})], "tool_use"
    summary = " ".join(f"{r[0]['input'].get('path')}: {r[1].strip()[:80]}" for r in reads)
    return [{"type": "text", "text": f"I read {len(reads)} file(s) in {folder}. {summary}"}], "end_turn"


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def reply(self, status, payload, extra=()):
        data = json.dumps(payload).encode()
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        for k, v in extra:
            self.send_header(k, v)
        self.end_headers()
        self.wfile.write(data)

    def do_POST(self):
        raw = self.rfile.read(int(self.headers["content-length"]))
        body = json.loads(raw)
        headers = {k.lower(): v for k, v in self.headers.items()}
        with lock:
            state["calls"] += 1
            n = state["calls"]
        scenario = self.server.scenario
        record = {"n": n, "path": self.path, "headers": {k: v for k, v in headers.items() if k != "x-api-key"},
                  "key_sent": bool(headers.get("x-api-key")), "body": body}
        status, payload, extra = 200, None, []
        try:
            if self.path != "/v1/messages":
                raise Bad(404, "not_found_error", "Not found")
            if scenario == "credit":
                raise Bad(400, "invalid_request_error", "Your credit balance is too low to access the Anthropic API. "
                                                       "Please go to Plans & Billing to upgrade or purchase credits.")
            if scenario == "billing":
                raise Bad(402, "billing_error", "There is a problem with your organization's billing.")
            previous = check(body, headers)
            turn = sum(1 for m in body["messages"] if m["role"] == "assistant")
            fault = scenario if scenario in ("overload2", "ratelimit2", "refusal2") and turn == 1 else None
            with lock:
                first_time = fault and fault not in state["faults"]
                if first_time:
                    state["faults"].add(fault)
            if fault == "overload2" and first_time:
                raise Bad(529, "overloaded_error", "Overloaded")
            if fault == "ratelimit2" and first_time:
                extra = [("retry-after", "1")]
                raise Bad(429, "rate_limit_error", "Number of request tokens has exceeded your per-minute rate limit")
            content = [{"type": "thinking", "thinking": "",
                        "signature": sign(body, body["messages"], previous)}]
            if fault == "refusal2":
                payload = {"id": f"msg_{n:03d}", "type": "message", "role": "assistant", "model": body["model"],
                           "content": [], "stop_reason": "refusal", "stop_sequence": None,
                           "stop_details": {"type": "refusal", "category": "cyber", "explanation": None},
                           "usage": usage(body, 0)}
            elif (scenario == "maxtokens1" and turn == 0) or scenario == "maxtokens":
                payload = {"id": f"msg_{n:03d}", "type": "message", "role": "assistant", "model": body["model"],
                           "content": content, "stop_reason": "max_tokens", "stop_sequence": None,
                           "usage": usage(body, body["max_tokens"])}
            else:
                more, stop = model(body, "desktop" if fault else scenario, previous, n)
                content += more
                payload = {"id": f"msg_{n:03d}", "type": "message", "role": "assistant", "model": body["model"],
                           "content": content, "stop_reason": stop, "stop_sequence": None,
                           "usage": usage(body, 40 + tokens(canonical(more)))}
        except Bad as e:
            status = e.status
            payload = {"type": "error", "error": {"type": e.kind, "message": e.message}, "request_id": f"req_fake_{n:03d}"}
        record["status"], record["reply"] = status, payload
        with open(self.server.log, "a") as f:
            f.write(json.dumps(record) + "\n")
        self.reply(status, payload, extra)


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--port", type=int, required=True)
    p.add_argument("--cert", required=True)
    p.add_argument("--key", required=True)
    p.add_argument("--log", required=True)
    p.add_argument("--scenario", default="desktop")
    a = p.parse_args()
    server = http.server.ThreadingHTTPServer(("127.0.0.1", a.port), Handler)
    server.scenario, server.log = a.scenario, a.log
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    ctx.load_cert_chain(a.cert, a.key)
    server.socket = ctx.wrap_socket(server.socket, server_side=True)
    print(f"fake anthropic on https://127.0.0.1:{a.port} ({a.scenario})", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    sys.exit(main())
