"""Whether an agent's page (GET /api/v1/agents/<id>, in the file named
first) shows task <id> ended or asking its person something: exit 0 if so.
The local-AWS tests wait on it."""

import json
import sys

detail = json.load(open(sys.argv[1]))
task = sys.argv[2]
for message in detail.get("messages", []):
    if message.get("task") != task:
        continue
    body = message.get("body", {})
    kind = body.get("kind")
    text = body.get("text", "")
    if kind in ("done", "approval") or text.startswith(("Couldn't finish", "Stopped")):
        sys.exit(0)
sys.exit(1)
