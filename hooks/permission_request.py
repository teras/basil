#!/usr/bin/env python3
"""
PermissionRequest hook for Basil.

This script is called by Claude Code when a permission is needed.
It posts the request to Basil server and waits for user decision.

Install:
1. Copy to ~/.claude/hooks/basil_permission.py
2. chmod +x ~/.claude/hooks/basil_permission.py
3. Add to ~/.claude/settings.json:
   {
     "hooks": {
       "PermissionRequest": [{
         "matcher": "*",
         "hooks": [{"type": "command", "command": "~/.claude/hooks/basil_permission.py"}]
       }]
     }
   }
"""

import json
import sys
import time
import urllib.request
import urllib.error
import os

BASIL_URL = os.environ.get("BASIL_URL", "http://localhost:8765")
TIMEOUT = int(os.environ.get("BASIL_PERMISSION_TIMEOUT", "120"))


LOG_FILE = "/tmp/basil_hook.log"

def log(msg):
    import datetime
    with open(LOG_FILE, "a") as f:
        f.write(f"[{datetime.datetime.now()}] {msg}\n")


def main():
    log("Hook called")

    # Read hook input from stdin
    try:
        raw = sys.stdin.read()
        log(f"Raw input: {raw[:500]}")
        input_data = json.loads(raw) if raw.strip() else {}
    except json.JSONDecodeError as e:
        log(f"JSON decode error: {e}")
        # No valid input, allow by default
        print(json.dumps({"behavior": "allow"}))
        return

    # Extract permission details
    tool_name = input_data.get("tool_name", "unknown")
    tool_input = input_data.get("tool_input", {})
    session_id = input_data.get("session_id", "")

    # Create a description of what's being requested
    if tool_name == "Bash":
        description = tool_input.get("command", "")[:200]
    elif tool_name == "Write":
        description = f"Write to {tool_input.get('file_path', 'unknown')}"
    elif tool_name == "Edit":
        description = f"Edit {tool_input.get('file_path', 'unknown')}"
    else:
        description = json.dumps(tool_input)[:200]

    # Generate a unique permission ID
    permission_id = f"perm_{int(time.time() * 1000)}"

    # Post permission request to Basil
    request_data = {
        "permission_id": permission_id,
        "tool_name": tool_name,
        "description": description,
        "tool_input": tool_input,
        "claude_session_id": session_id,
    }

    log(f"Posting to {BASIL_URL}/hook/permission: {request_data}")

    try:
        req = urllib.request.Request(
            f"{BASIL_URL}/hook/permission",
            data=json.dumps(request_data).encode(),
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        with urllib.request.urlopen(req, timeout=5) as resp:
            result = json.loads(resp.read())
            log(f"Server response: {result}")
            if not result.get("ok"):
                # Server rejected, allow by default
                log("Server rejected, allowing")
                print(json.dumps({"behavior": "allow"}))
                return
    except Exception as e:
        # Can't reach server, allow by default
        log(f"Server error: {e}")
        sys.stderr.write(f"Basil server error: {e}\n")
        print(json.dumps({"behavior": "allow"}))
        return

    # Poll for decision
    start_time = time.time()
    while time.time() - start_time < TIMEOUT:
        try:
            req = urllib.request.Request(
                f"{BASIL_URL}/hook/permission/{permission_id}/decision",
                method="GET",
            )
            with urllib.request.urlopen(req, timeout=5) as resp:
                result = json.loads(resp.read())

                if result.get("decided"):
                    decision = result.get("decision", "deny")
                    log(f"Got decision: {decision}")
                    if decision == "allow":
                        print(json.dumps({"behavior": "allow"}))
                    else:
                        print(json.dumps({
                            "behavior": "deny",
                            "message": result.get("message", "Denied by user via Basil")
                        }))
                    return

        except urllib.error.HTTPError as e:
            if e.code == 404:
                # Permission request expired or not found
                log("Permission expired/not found")
                print(json.dumps({"behavior": "deny", "message": "Permission request expired"}))
                return
        except Exception as ex:
            log(f"Poll error: {ex}")

        time.sleep(1)

    # Timeout - deny by default
    log("Timeout reached")
    print(json.dumps({"behavior": "deny", "message": "Permission request timed out"}))


if __name__ == "__main__":
    main()
