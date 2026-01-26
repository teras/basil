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
) -> AsyncIterator[ResponseBlock]:
    """Run Claude Code CLI and yield response blocks."""

    # Build command
    cmd = [
        "claude",
        "-p",  # Print mode (non-interactive)
        "--output-format", "stream-json",
        "--verbose",  # Required for stream-json
    ]

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

        # Read streaming JSON output
        block_id = 0
        last_text = ""

        async for line in process.stdout:
            line = line.decode().strip()
            if not line:
                continue

            try:
                data = json.loads(line)
            except json.JSONDecodeError:
                continue

            msg_type = data.get("type")

            if msg_type == "system" and data.get("subtype") == "init":
                # Extract session ID from init message
                sid = data.get("session_id")
                if sid:
                    session.claude_session_id = sid

            elif msg_type == "assistant":
                # Assistant message with content
                content_list = data.get("message", {}).get("content", [])
                for content in content_list:
                    if content.get("type") == "text":
                        text = content.get("text", "")
                        # Only yield if text changed (streaming updates same message)
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
                        # Tool being used
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
                # Final result
                result_text = data.get("result", "")
                session_id = data.get("session_id")

                if session_id:
                    session.claude_session_id = session_id

                # If we got a final result different from last text, yield it
                if result_text and result_text != last_text:
                    block_id += 1
                    yield ResponseBlock(
                        block_id=block_id,
                        content=result_text,
                        block_type="text",
                        more=False,
                    )
                else:
                    # Just mark as done
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
