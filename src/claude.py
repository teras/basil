"""Claude Code CLI wrapper."""

import asyncio
import json
import os
import signal
from typing import AsyncIterator

from .sessions import Session, ResponseBlock


async def run_claude(
    session: Session,
    message: str,
    plan_mode: bool = False,
) -> AsyncIterator[ResponseBlock]:
    """Run Claude Code CLI and yield response blocks."""

    # Build command
    cmd = [
        "claude",
        "-p",  # Print mode (non-interactive)
        "--output-format", "stream-json",
        "--verbose",  # Required for stream-json
    ]

    # Add permission mode
    if plan_mode:
        # Plan mode: read-only, no file modifications
        cmd.extend(["--permission-mode", "plan"])
    else:
        # Execute mode: allow all operations (safe in Docker isolation)
        cmd.append("--dangerously-skip-permissions")

    # Add continue flag if we have a previous session
    if session.claude_session_id:
        cmd.extend(["--resume", session.claude_session_id])

    # Create process
    process = await asyncio.create_subprocess_exec(
        *cmd,
        stdin=asyncio.subprocess.PIPE,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
        cwd=session.working_dir,
        env={**os.environ, "NO_COLOR": "1"},
    )

    # Store process for potential cancellation
    session._claude_process = process

    try:
        # Send message to stdin
        process.stdin.write(message.encode())
        await process.stdin.drain()
        process.stdin.close()

        # Read streaming JSON output - no size limits
        block_id = 0
        last_text = ""
        buffer = b""

        while True:
            # Read whatever is available (no limit)
            chunk = await process.stdout.read(1024 * 1024)  # 1MB at a time
            if not chunk:
                break
            buffer += chunk

            # Process complete lines
            while b"\n" in buffer:
                line_bytes, buffer = buffer.split(b"\n", 1)
                try:
                    line = line_bytes.decode("utf-8").strip()
                except UnicodeDecodeError:
                    continue

                if not line:
                    continue

                try:
                    data = json.loads(line)
                except json.JSONDecodeError:
                    continue

                msg_type = data.get("type")

                if msg_type == "system" and data.get("subtype") == "init":
                    sid = data.get("session_id")
                    if sid:
                        session.claude_session_id = sid

                elif msg_type == "assistant":
                    content_list = data.get("message", {}).get("content", [])
                    for content in content_list:
                        if content.get("type") == "text":
                            text = content.get("text", "")
                            if text and text != last_text:
                                last_text = text
                                block_id += 1
                                yield ResponseBlock(
                                    block_id=block_id,
                                    content=text,
                                    block_type="text",
                                    more=True,
                                )
                        elif content.get("type") == "tool_use":
                            tool_name = content.get("name", "unknown")
                            tool_input = content.get("input", {})
                            block_id += 1
                            yield ResponseBlock(
                                block_id=block_id,
                                content=f"Using tool: {tool_name}",
                                block_type="tool",
                                more=True,
                                metadata={"tool": tool_name, "input": tool_input}
                            )

                elif msg_type == "result":
                    result_text = data.get("result", "")
                    session_id = data.get("session_id")

                    if session_id:
                        session.claude_session_id = session_id

                    if result_text and result_text != last_text:
                        block_id += 1
                        yield ResponseBlock(
                            block_id=block_id,
                            content=result_text,
                            block_type="text",
                            more=False,
                        )
                    else:
                        block_id += 1
                        yield ResponseBlock(
                            block_id=block_id,
                            content="",
                            block_type="done",
                            more=False,
                        )

                elif msg_type == "error":
                    error_msg = data.get("error", {}).get("message", str(data))
                    block_id += 1
                    yield ResponseBlock(
                        block_id=block_id,
                        content=f"Error: {error_msg}",
                        block_type="error",
                        more=False,
                    )


        # Wait for process to complete
        await process.wait()

        # Read any stderr
        stderr = await process.stderr.read()
        if stderr and process.returncode != 0:
            block_id += 1
            yield ResponseBlock(
                block_id=block_id,
                content=f"Claude error: {stderr.decode()[:500]}",
                block_type="error",
                more=False,
            )

    except asyncio.CancelledError:
        # Kill the process if cancelled
        try:
            process.send_signal(signal.SIGTERM)
            await asyncio.wait_for(process.wait(), timeout=5)
        except:
            process.kill()
        raise

    finally:
        session._claude_process = None


async def stop_claude(session: Session) -> bool:
    """Stop a running Claude process."""
    process = getattr(session, "_claude_process", None)
    if process:
        try:
            process.send_signal(signal.SIGTERM)
            await asyncio.wait_for(process.wait(), timeout=5)
            return True
        except:
            try:
                process.kill()
                return True
            except:
                pass
    return False
