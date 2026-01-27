"""Basil - Clean HTTP API for Claude Code chat."""

import asyncio
from contextlib import asynccontextmanager
from pathlib import Path
from typing import Annotated

import uvicorn
from fastapi import FastAPI, APIRouter, HTTPException, Header, BackgroundTasks
from fastapi.responses import JSONResponse, HTMLResponse
from pydantic import BaseModel

from .config import get_settings
from .sessions import sessions, Session, ResponseBlock

# UI HTML loaded from file
_UI_HTML: str | None = None

def get_ui_html() -> str:
    """Load UI HTML from file (cached)."""
    global _UI_HTML
    if _UI_HTML is None:
        ui_path = Path(__file__).parent / "ui.html"
        _UI_HTML = ui_path.read_text()
    return _UI_HTML


# ============================================================================
# Request/Response Models
# ============================================================================

class NewSessionRequest(BaseModel):
    working_dir: str | None = None
    name: str | None = None


class ChatRequest(BaseModel):
    text: str
    plan_mode: bool = False


class SimpleRequest(BaseModel):
    prompt: str
    working_dir: str = "/workspace"


class RenameSessionRequest(BaseModel):
    name: str


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

# API router for detailed endpoints
api = APIRouter(prefix="/api")


def get_session_or_error(session_id: str) -> Session:
    """Get session or raise 404."""
    session = sessions.get_session(session_id)
    if not session:
        raise HTTPException(status_code=404, detail="Session not found")
    return session


# ============================================================================
# Session Endpoints
# ============================================================================

@api.post("/session/new")
async def create_session(req: NewSessionRequest):
    """Create a new chat session."""
    session = sessions.create_session(working_dir=req.working_dir)
    return {
        "session_id": session.session_id,
        "working_dir": session.working_dir,
        "name": session.name,
        "status": "ready"
    }


@api.get("/session/list")
async def list_sessions():
    """List all sessions."""
    return {"sessions": sessions.list_sessions()}


@api.get("/session/{session_id}")
async def get_session_info(session_id: str):
    """Get session details including chat history."""
    session = get_session_or_error(session_id)
    return {
        "session_id": session.session_id,
        "working_dir": session.working_dir,
        "created_at": session.created_at,
        "name": session.name,
        "is_processing": session.is_processing,
        "messages": session.messages,
        "plan_mode": session.plan_mode,
    }


@api.post("/session/{session_id}/cd")
async def change_directory(session_id: str, req: ChatRequest):
    """Change session working directory."""
    session = get_session_or_error(session_id)
    # TODO: Validate path exists
    session.working_dir = req.text
    sessions.update_session(session)
    return {"working_dir": session.working_dir}


@api.delete("/session/{session_id}")
async def delete_session(session_id: str):
    """Delete a session."""
    if sessions.delete_session(session_id):
        return {"deleted": True}
    raise HTTPException(status_code=404, detail="Session not found")


@api.patch("/session/{session_id}/rename")
async def rename_session(session_id: str, req: RenameSessionRequest):
    """Rename a session."""
    session = get_session_or_error(session_id)
    session.name = req.name
    sessions.update_session(session)
    return {"ok": True, "name": session.name}


class SetModeRequest(BaseModel):
    plan_mode: bool


@api.patch("/session/{session_id}/mode")
async def set_session_mode(session_id: str, req: SetModeRequest):
    """Set session plan/execute mode."""
    session = get_session_or_error(session_id)
    session.plan_mode = req.plan_mode
    sessions.update_session(session)
    return {"ok": True, "plan_mode": session.plan_mode}


# ============================================================================
# Chat Endpoints
# ============================================================================

@api.post("/chat")
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

    # Save user message
    session.add_message("user", req.text)
    session.is_processing = True

    # Start Claude processing in background
    background_tasks.add_task(process_claude_message, session, req.text, req.plan_mode)

    return {"status": "processing", "session_id": session.session_id}


@api.get("/chat/next")
async def get_next_block(
    x_session: Annotated[str, Header()],
    timeout: int = 30,
):
    """Get the next response block. Blocks until available or timeout."""
    session = get_session_or_error(x_session)

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


@api.post("/chat/stop")
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
# Simple Root Endpoint
# ============================================================================


@app.get("/health")
async def health_check():
    """Health check endpoint."""
    return {"status": "ok", "service": "basil"}


@app.get("/project")
async def get_project_info():
    """Get project information."""
    settings = get_settings()
    return {"name": settings.project_name, "path": settings.project_path}


@app.post("/")
async def simple_chat(req: SimpleRequest):
    """Simple synchronous chat - send prompt, get full response."""
    # Create a temporary session
    session = sessions.create_session(working_dir=req.working_dir)

    try:
        # Run Claude and collect all responses
        from .claude import run_claude

        response_text = []
        tools_used = []

        async for block in run_claude(session, req.prompt):
            if block.block_type == "text" and block.content:
                response_text.append(block.content)
            elif block.block_type == "tool":
                tools_used.append({
                    "tool": block.metadata.get("tool") if block.metadata else None,
                    "input": block.metadata.get("input") if block.metadata else None,
                })

            if not block.more:
                break

        return {
            "response": "\n".join(response_text),
            "tools_used": tools_used,
            "session_id": session.session_id,
        }
    except Exception as e:
        return JSONResponse(
            status_code=500,
            content={"error": str(e)}
        )


# Mount API router
app.include_router(api)


# Conditionally mount Web UI
def _add_ui_route():
    """Add web UI route if enabled."""
    settings = get_settings()
    if settings.serve_ui:
        @app.get("/", response_class=HTMLResponse)
        async def chat_ui():
            """Serve the chat UI."""
            return get_ui_html()

_add_ui_route()


# ============================================================================
# Claude Processing
# ============================================================================

async def process_claude_message(session: Session, message: str, plan_mode: bool = False):
    """Process a message through Claude Code CLI."""
    from .claude import run_claude

    try:
        last_text = ""
        async for block in run_claude(session, message, plan_mode=plan_mode):
            # Update block_id to use session's counter
            block.block_id = session.next_block_id()
            await session.response_queue.put(block)

            # Save each distinct text block as a separate message
            if block.block_type == "text" and block.content:
                if block.content != last_text:
                    session.add_message("assistant", block.content)
                    last_text = block.content

            # Save tool actions too
            if block.block_type == "tool":
                import json
                tool_name = block.metadata.get("tool", "tool") if block.metadata else "tool"
                tool_input = block.metadata.get("input", {}) if block.metadata else {}
                session.add_message("tool", json.dumps({"tool": tool_name, "input": tool_input}))

            # If this was the last block, we're done
            if not block.more:
                break

        # Save session with updated claude_session_id and messages
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

class HealthCheckFilter:
    """Filter out /health requests from access logs."""
    def __init__(self, app):
        self.app = app

    async def __call__(self, scope, receive, send):
        if scope["type"] == "http" and scope["path"] == "/health":
            # Skip logging for health checks
            scope["state"] = scope.get("state", {})
            scope["state"]["skip_log"] = True
        return await self.app(scope, receive, send)


def main():
    """Run the server."""
    import logging

    # Filter out /health from uvicorn access logs
    class EndpointFilter(logging.Filter):
        def filter(self, record: logging.LogRecord) -> bool:
            return "/health" not in record.getMessage()

    logging.getLogger("uvicorn.access").addFilter(EndpointFilter())

    settings = get_settings()
    uvicorn.run(
        "src.main:app",
        host=settings.host,
        port=settings.port,
        reload=False,
    )


if __name__ == "__main__":
    main()
