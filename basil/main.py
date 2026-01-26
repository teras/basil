"""Basil - Clean HTTP API for Claude Code chat."""

import asyncio
from contextlib import asynccontextmanager
from typing import Annotated

import uvicorn
from fastapi import FastAPI, HTTPException, Header, BackgroundTasks
from fastapi.responses import JSONResponse
from pydantic import BaseModel

from .config import get_settings
from .sessions import sessions, Session, ResponseBlock
from .permissions import permissions, PermissionRequest


# ============================================================================
# Request/Response Models
# ============================================================================

class NewSessionRequest(BaseModel):
    working_dir: str | None = None
    name: str | None = None


class ChatRequest(BaseModel):
    text: str


class PermissionResponse(BaseModel):
    allow: bool
    message: str | None = None


class HookPermissionRequest(BaseModel):
    permission_id: str
    tool_name: str
    description: str
    tool_input: dict = {}
    claude_session_id: str = ""


# ============================================================================
# Application Setup
# ============================================================================

@asynccontextmanager
async def lifespan(app: FastAPI):
    """Application lifespan - startup and shutdown."""
    settings = get_settings()
    print(f"Basil starting on {settings.host}:{settings.port}")
    yield
    print("Basil stopped")


app = FastAPI(
    title="Basil",
    description="Clean HTTP API for Claude Code",
    lifespan=lifespan,
)


def get_session_or_error(session_id: str) -> Session:
    """Get session or raise 404."""
    session = sessions.get_session(session_id)
    if not session:
        raise HTTPException(status_code=404, detail="Session not found")
    return session


# ============================================================================
# Session Endpoints
# ============================================================================

@app.post("/session/new")
async def create_session(req: NewSessionRequest):
    """Create a new chat session."""
    session = sessions.create_session(working_dir=req.working_dir)
    return {
        "session_id": session.session_id,
        "working_dir": session.working_dir,
        "status": "ready"
    }


@app.get("/session/list")
async def list_sessions():
    """List all sessions."""
    return {"sessions": sessions.list_sessions()}


@app.get("/session/{session_id}")
async def get_session_info(session_id: str):
    """Get session details."""
    session = get_session_or_error(session_id)
    return {
        "session_id": session.session_id,
        "working_dir": session.working_dir,
        "created_at": session.created_at,
        "is_processing": session.is_processing,
        "has_pending_permission": session.pending_permission is not None,
    }


@app.post("/session/{session_id}/cd")
async def change_directory(session_id: str, req: ChatRequest):
    """Change session working directory."""
    session = get_session_or_error(session_id)
    # TODO: Validate path exists
    session.working_dir = req.text
    sessions.update_session(session)
    return {"working_dir": session.working_dir}


@app.delete("/session/{session_id}")
async def delete_session(session_id: str):
    """Delete a session."""
    if sessions.delete_session(session_id):
        return {"deleted": True}
    raise HTTPException(status_code=404, detail="Session not found")


# ============================================================================
# Chat Endpoints
# ============================================================================

@app.post("/chat")
async def send_message(
    req: ChatRequest,
    background_tasks: BackgroundTasks,
    x_session: Annotated[str, Header()],
):
    """Send a message to Claude."""
    session = get_session_or_error(x_session)

    if session.is_processing:
        raise HTTPException(status_code=409, detail="Session is busy processing")

    # Clear any old responses
    while not session.response_queue.empty():
        try:
            session.response_queue.get_nowait()
        except asyncio.QueueEmpty:
            break

    session.is_processing = True

    # Start Claude processing in background
    background_tasks.add_task(process_claude_message, session, req.text)

    return {"status": "processing", "session_id": session.session_id}


@app.get("/chat/next")
async def get_next_block(
    x_session: Annotated[str, Header()],
    timeout: int = 30,
):
    """Get the next response block. Blocks until available or timeout."""
    session = get_session_or_error(x_session)

    # Check for pending permission first
    if session.pending_permission:
        return {
            "type": "permission",
            "permission_id": session.pending_permission["id"],
            "tool": session.pending_permission["tool"],
            "description": session.pending_permission["description"],
            "more": True,
        }

    try:
        block: ResponseBlock = await asyncio.wait_for(
            session.response_queue.get(),
            timeout=timeout
        )
        return block.to_dict()
    except asyncio.TimeoutError:
        return {
            "type": "timeout",
            "more": session.is_processing,
            "message": "No response yet, try again"
        }


@app.post("/chat/stop")
async def stop_processing(x_session: Annotated[str, Header()]):
    """Stop the current Claude operation."""
    from .claude import stop_claude

    session = get_session_or_error(x_session)

    stopped = await stop_claude(session)
    session.is_processing = False

    # Add a stop message to the queue
    await session.response_queue.put(ResponseBlock(
        block_id=session.next_block_id(),
        content="Stopped by user",
        block_type="system",
        more=False,
    ))

    return {"stopped": stopped}


# ============================================================================
# Permission Endpoints
# ============================================================================

@app.post("/permission/{permission_id}")
async def respond_to_permission(
    permission_id: str,
    req: PermissionResponse,
    x_session: Annotated[str, Header()],
):
    """Respond to a permission request."""
    session = get_session_or_error(x_session)

    if not session.pending_permission:
        raise HTTPException(status_code=404, detail="No pending permission")

    if session.pending_permission["id"] != permission_id:
        raise HTTPException(status_code=400, detail="Permission ID mismatch")

    # Store the decision (the Claude process will pick it up)
    session.pending_permission["decision"] = "allow" if req.allow else "deny"

    return {"ok": True, "decision": session.pending_permission["decision"]}


# ============================================================================
# Hook Endpoints (for PermissionRequest hook)
# ============================================================================

@app.post("/hook/permission")
async def hook_permission_request(req: HookPermissionRequest):
    """Receive a permission request from the Claude hook."""
    # Create permission request
    perm_req = PermissionRequest(
        permission_id=req.permission_id,
        tool_name=req.tool_name,
        description=req.description,
        tool_input=req.tool_input,
        claude_session_id=req.claude_session_id,
    )
    permissions.add(perm_req)

    # Find the Basil session associated with this Claude session
    # and notify it about the pending permission
    for sid, session in sessions._sessions.items():
        if session.claude_session_id == req.claude_session_id:
            # Add permission block to the response queue
            await session.response_queue.put(ResponseBlock(
                block_id=session.next_block_id(),
                content=f"Permission required: {req.tool_name}",
                block_type="permission",
                more=True,
                metadata={
                    "permission_id": req.permission_id,
                    "tool": req.tool_name,
                    "description": req.description,
                }
            ))
            break

    return {"ok": True, "permission_id": req.permission_id}


@app.get("/hook/permission/{permission_id}/decision")
async def hook_get_permission_decision(permission_id: str):
    """Hook polls this endpoint to get the user's decision."""
    perm_req = permissions.get(permission_id)
    if not perm_req:
        raise HTTPException(status_code=404, detail="Permission not found")

    return {
        "decided": perm_req.decided,
        "decision": perm_req.decision,
        "message": perm_req.message,
    }


@app.post("/permission/{permission_id}/decide")
async def decide_permission(
    permission_id: str,
    req: PermissionResponse,
):
    """User decides on a permission request."""
    perm_req = permissions.get(permission_id)
    if not perm_req:
        raise HTTPException(status_code=404, detail="Permission not found")

    if perm_req.decided:
        raise HTTPException(status_code=400, detail="Permission already decided")

    perm_req.set_decision(req.allow, req.message)

    return {"ok": True, "decision": perm_req.decision}


# ============================================================================
# Health Check
# ============================================================================

@app.get("/")
async def health_check():
    """Health check endpoint."""
    return {"status": "ok", "service": "basil"}


# ============================================================================
# Claude Processing
# ============================================================================

async def process_claude_message(session: Session, message: str):
    """Process a message through Claude Code CLI."""
    from .claude import run_claude

    try:
        async for block in run_claude(session, message):
            # Update block_id to use session's counter
            block.block_id = session.next_block_id()
            await session.response_queue.put(block)

            # If this was the last block, we're done
            if not block.more:
                break

        # Save session with updated claude_session_id
        from .sessions import sessions
        sessions.update_session(session)

    except asyncio.CancelledError:
        await session.response_queue.put(ResponseBlock(
            block_id=session.next_block_id(),
            content="Cancelled",
            block_type="system",
            more=False,
        ))
    except Exception as e:
        await session.response_queue.put(ResponseBlock(
            block_id=session.next_block_id(),
            content=f"Error: {e}",
            block_type="error",
            more=False,
        ))
    finally:
        session.is_processing = False


# ============================================================================
# Entry Point
# ============================================================================

def main():
    """Run the server."""
    settings = get_settings()
    uvicorn.run(
        "basil.main:app",
        host=settings.host,
        port=settings.port,
        reload=False,
    )


if __name__ == "__main__":
    main()
