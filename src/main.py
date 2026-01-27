"""Basil - Clean HTTP API for Claude Code chat."""

import asyncio
from contextlib import asynccontextmanager
from typing import Annotated

import uvicorn
from fastapi import FastAPI, APIRouter, HTTPException, Header, BackgroundTasks
from fastapi.responses import JSONResponse, HTMLResponse
from pydantic import BaseModel

from .config import get_settings
from .sessions import sessions, Session, ResponseBlock


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

CHAT_HTML = """
<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Basil Chat</title>
    <script src="https://cdn.jsdelivr.net/npm/marked/marked.min.js"></script>
    <link rel="stylesheet" href="https://cdnjs.cloudflare.com/ajax/libs/highlight.js/11.11.1/styles/github-dark.min.css">
    <script src="https://cdnjs.cloudflare.com/ajax/libs/highlight.js/11.11.1/highlight.min.js"></script>
    <style>
        * {
            margin: 0;
            padding: 0;
            box-sizing: border-box;
        }

        body {
            font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, Oxygen, Ubuntu, sans-serif;
            background: linear-gradient(135deg, #1a1a2e 0%, #16213e 50%, #0f3460 100%);
            height: 100vh;
            overflow: hidden;
            display: flex;
            flex-direction: column;
        }

        .header {
            background: rgba(255,255,255,0.05);
            backdrop-filter: blur(10px);
            padding: 1rem 2rem;
            border-bottom: 1px solid rgba(255,255,255,0.1);
            display: flex;
            align-items: center;
            gap: 1rem;
            flex-shrink: 0;
            position: relative;
            z-index: 100;
        }

        .header h1 {
            color: #fff;
            font-size: 1.5rem;
            font-weight: 600;
            margin-left: auto;
        }

        .project-name {
            position: absolute;
            left: 50%;
            transform: translateX(-50%);
            color: rgba(255,255,255,0.85);
            font-size: 1.4rem;
            font-weight: 600;
        }

        .header .logo {
            font-size: 2rem;
        }

        .header-left {
            display: flex;
            align-items: center;
            gap: 0.5rem;
        }

        .header-actions {
            display: flex;
            align-items: center;
            gap: 1rem;
        }

        .session-picker {
            position: relative;
        }

        .session-btn {
            background: rgba(255,255,255,0.1);
            border: 1px solid rgba(255,255,255,0.2);
            border-radius: 0.5rem;
            padding: 0.5rem 1rem;
            color: white;
            font-size: 0.85rem;
            cursor: pointer;
            display: flex;
            align-items: center;
            gap: 0.5rem;
            transition: all 0.2s;
        }

        .session-btn:hover {
            background: rgba(255,255,255,0.15);
        }

        .session-dropdown {
            position: absolute;
            top: 100%;
            left: 0;
            margin-top: 0.5rem;
            background: #1e293b;
            border: 1px solid rgba(255,255,255,0.1);
            border-radius: 0.5rem;
            min-width: 300px;
            max-height: 400px;
            overflow-y: auto;
            display: none;
            z-index: 10;
            box-shadow: 0 10px 40px rgba(0,0,0,0.5), 0 0 0 1px rgba(255,255,255,0.1);
        }

        .session-dropdown.open {
            display: block;
        }

        .session-dropdown-header {
            padding: 0.75rem 1rem;
            border-bottom: 1px solid rgba(255,255,255,0.1);
            font-weight: 600;
            color: rgba(255,255,255,0.8);
            display: flex;
            justify-content: space-between;
            align-items: center;
        }

        .new-session-btn {
            background: linear-gradient(135deg, #667eea 0%, #764ba2 100%);
            border: none;
            border-radius: 0.25rem;
            padding: 0.3rem 0.6rem;
            color: white;
            font-size: 0.75rem;
            cursor: pointer;
        }

        .session-item {
            padding: 0.75rem 1rem;
            border-bottom: 1px solid rgba(255,255,255,0.05);
            cursor: pointer;
            display: flex;
            justify-content: space-between;
            align-items: center;
            transition: background 0.2s;
        }

        .session-item:hover {
            background: rgba(255,255,255,0.05);
        }

        .session-item.active {
            background: rgba(102, 126, 234, 0.2);
        }

        .session-info {
            flex: 1;
        }

        .session-info .session-name {
            font-size: 0.9rem;
            color: #e2e8f0;
            display: flex;
            align-items: center;
            gap: 0.4rem;
        }

        .processing-dot {
            width: 8px;
            height: 8px;
            background: #fbbf24;
            border-radius: 50%;
            animation: pulse 1s infinite;
        }

        .session-actions {
            display: flex;
            gap: 0.25rem;
            opacity: 0;
            transition: opacity 0.2s;
        }

        .session-item:hover .session-actions {
            opacity: 1;
        }

        .session-rename {
            background: rgba(102, 126, 234, 0.2);
            border: none;
            border-radius: 0.25rem;
            padding: 0.3rem 0.5rem;
            color: #667eea;
            font-size: 0.75rem;
            cursor: pointer;
            transition: background 0.2s;
        }

        .session-rename:hover {
            background: rgba(102, 126, 234, 0.4);
        }

        .session-info .session-name-input {
            background: rgba(0,0,0,0.3);
            border: 1px solid #667eea;
            border-radius: 0.25rem;
            padding: 0.2rem 0.4rem;
            color: white;
            font-size: 0.9rem;
            width: 100%;
            outline: none;
        }

        .session-info .session-dir {
            font-size: 0.75rem;
            color: rgba(255,255,255,0.5);
            margin-top: 0.2rem;
        }

        .session-delete {
            background: rgba(239, 68, 68, 0.2);
            border: none;
            border-radius: 0.25rem;
            padding: 0.3rem 0.5rem;
            color: #ef4444;
            font-size: 0.75rem;
            cursor: pointer;
            transition: background 0.2s;
        }

        .session-delete:hover {
            background: rgba(239, 68, 68, 0.4);
        }

        .no-sessions {
            padding: 1rem;
            text-align: center;
            color: rgba(255,255,255,0.4);
            font-size: 0.85rem;
        }

        .plan-mode-btn {
            background: rgba(59, 130, 246, 0.2);
            border: 1px solid rgba(59, 130, 246, 0.5);
            border-radius: 0.5rem;
            padding: 0.5rem;
            color: #3b82f6;
            font-size: 1.1rem;
            cursor: pointer;
            display: flex;
            align-items: center;
            justify-content: center;
            transition: all 0.2s;
            width: 36px;
            height: 36px;
        }

        .plan-mode-btn:hover {
            background: rgba(59, 130, 246, 0.3);
        }

        .plan-mode-btn.unlocked {
            background: rgba(239, 68, 68, 0.2);
            border-color: rgba(239, 68, 68, 0.5);
            color: #ef4444;
        }

        .status-circle {
            width: 16px;
            height: 16px;
            border-radius: 50%;
            background: #fbbf24;
            flex-shrink: 0;
            margin-right: 0.75rem;
        }

        .status-circle.connected {
            background: #4ade80;
            box-shadow: 0 0 12px rgba(74, 222, 128, 0.5);
            animation: pulse 2s infinite;
        }

        .status-circle.disconnected {
            background: #ef4444;
            box-shadow: 0 0 12px rgba(239, 68, 68, 0.5);
        }

        @keyframes pulse {
            0%, 100% { opacity: 1; }
            50% { opacity: 0.5; }
        }

        .chat-container {
            flex: 1;
            overflow-y: auto;
            padding: 2rem;
            display: flex;
            flex-direction: column;
            gap: 1rem;
            position: relative;
            z-index: 1;
        }

        .message {
            max-width: 85%;
            padding: 1rem 1.25rem;
            border-radius: 1.25rem;
            animation: slideIn 0.3s ease;
            line-height: 1.5;
            word-wrap: break-word;
        }

        @keyframes slideIn {
            from {
                opacity: 0;
                transform: translateY(10px);
            }
            to {
                opacity: 1;
                transform: translateY(0);
            }
        }

        .message.user {
            align-self: flex-end;
            background: linear-gradient(135deg, #667eea 0%, #764ba2 100%);
            color: white;
            border-bottom-right-radius: 0.25rem;
            box-shadow: 0 4px 15px rgba(102, 126, 234, 0.3);
        }

        .message.assistant {
            align-self: flex-start;
            background: rgba(30,41,59,0.95);
            color: #e2e8f0;
            border-bottom-left-radius: 0.25rem;
            border: 1px solid rgba(255,255,255,0.1);
        }

        .message.assistant pre {
            background: rgba(0,0,0,0.4);
            padding: 1rem;
            border-radius: 0.5rem;
            overflow-x: auto;
            margin: 0.75rem 0;
            font-family: 'Monaco', 'Menlo', 'Consolas', monospace;
            font-size: 0.85rem;
            border: 1px solid rgba(255,255,255,0.1);
        }

        .message.assistant pre code {
            background: none;
            padding: 0;
            border-radius: 0;
            font-size: inherit;
        }

        .message.assistant code {
            background: rgba(0,0,0,0.3);
            padding: 0.2rem 0.5rem;
            border-radius: 0.25rem;
            font-family: 'Monaco', 'Menlo', 'Consolas', monospace;
            font-size: 0.85rem;
        }

        .message.assistant p {
            margin: 0.5rem 0;
        }

        .message.assistant p:first-child {
            margin-top: 0;
        }

        .message.assistant p:last-child {
            margin-bottom: 0;
        }

        .message.assistant ul, .message.assistant ol {
            margin: 0.5rem 0;
            padding-left: 1.5rem;
        }

        .message.assistant li {
            margin: 0.25rem 0;
        }

        .message.system {
            align-self: center;
            background: rgba(251, 191, 36, 0.2);
            color: #fbbf24;
            font-size: 0.85rem;
            padding: 0.5rem 1rem;
            border-radius: 1rem;
        }

        .message.tool-message {
            padding: 0.5rem 1rem;
            background: rgba(30,41,59,0.7);
        }

        .message.tool-message .tool-actions {
            margin: 0;
            padding: 0;
            border: none;
        }

        .tool-actions {
            margin-bottom: 0.75rem;
            padding-bottom: 0.75rem;
            border-bottom: 1px solid rgba(255,255,255,0.1);
        }

        .tool-action {
            display: flex;
            align-items: center;
            gap: 0.5rem;
            padding: 0.3rem 0;
            font-size: 0.8rem;
        }

        .tool-action .tool-icon {
            font-size: 0.9rem;
        }

        .tool-action .tool-name {
            color: #60a5fa;
            font-weight: 600;
        }

        .tool-action .tool-detail {
            color: rgba(226, 232, 240, 0.7);
            font-family: monospace;
            font-size: 0.75rem;
            overflow: hidden;
            text-overflow: ellipsis;
            white-space: nowrap;
            max-width: 350px;
        }

        .input-container {
            background: rgba(255,255,255,0.05);
            backdrop-filter: blur(10px);
            padding: 1.5rem 2rem;
            border-top: 1px solid rgba(255,255,255,0.1);
            flex-shrink: 0;
        }

        .input-wrapper {
            display: flex;
            gap: 1rem;
            max-width: 900px;
            margin: 0 auto;
            align-items: flex-end;
        }

        .scroll-buttons {
            display: flex;
            flex-direction: column;
            gap: 0.25rem;
        }

        .scroll-btn {
            background: rgba(255,255,255,0.1);
            border: 1px solid rgba(255,255,255,0.2);
            border-radius: 0.25rem;
            width: 2rem;
            height: 1.5rem;
            color: rgba(255,255,255,0.6);
            cursor: pointer;
            font-size: 0.8rem;
            transition: all 0.2s;
            display: flex;
            align-items: center;
            justify-content: center;
        }

        .scroll-btn:hover {
            background: rgba(255,255,255,0.2);
            color: white;
        }

        #messageInput {
            flex: 1;
            background: rgba(255,255,255,0.1);
            border: 1px solid rgba(255,255,255,0.2);
            border-radius: 1.5rem;
            padding: 1rem 1.5rem;
            color: white;
            font-size: 1rem;
            outline: none;
            transition: border-color 0.3s, box-shadow 0.3s;
            resize: none;
            overflow-y: hidden;
            min-height: 52px;
            max-height: 200px;
            line-height: 1.4;
            font-family: inherit;
        }

        #messageInput:focus {
            border-color: #667eea;
            box-shadow: 0 0 0 3px rgba(102, 126, 234, 0.2);
        }

        #messageInput::placeholder {
            color: rgba(255,255,255,0.4);
        }

        #sendBtn {
            background: linear-gradient(135deg, #667eea 0%, #764ba2 100%);
            border: none;
            border-radius: 1.5rem;
            padding: 1rem 2rem;
            color: white;
            font-size: 1rem;
            font-weight: 600;
            cursor: pointer;
            transition: all 0.3s;
            display: flex;
            align-items: center;
            gap: 0.5rem;
        }

        #sendBtn:hover {
            transform: translateY(-2px);
            box-shadow: 0 4px 20px rgba(102, 126, 234, 0.4);
        }

        #sendBtn:disabled {
            opacity: 0.5;
            cursor: not-allowed;
            transform: none;
        }

        .typing-indicator {
            display: none;
            align-self: flex-start;
            background: rgba(255,255,255,0.1);
            padding: 1rem 1.5rem;
            border-radius: 1.25rem;
            border-bottom-left-radius: 0.25rem;
        }

        .typing-indicator.active {
            display: flex;
            gap: 0.3rem;
        }

        .typing-indicator span {
            width: 8px;
            height: 8px;
            background: #667eea;
            border-radius: 50%;
            animation: bounce 1.4s infinite;
        }

        .typing-indicator span:nth-child(2) { animation-delay: 0.2s; }
        .typing-indicator span:nth-child(3) { animation-delay: 0.4s; }

        @keyframes bounce {
            0%, 60%, 100% { transform: translateY(0); }
            30% { transform: translateY(-8px); }
        }

        .empty-state {
            flex: 1;
            display: flex;
            flex-direction: column;
            align-items: center;
            justify-content: center;
            color: rgba(255,255,255,0.4);
            gap: 1rem;
        }

        .empty-state .icon {
            font-size: 4rem;
        }
    </style>
</head>
<body>
    <div class="header">
        <div class="header-left">
            <div class="status-circle" id="connectionStatus" title="Connecting..."></div>
            <button id="planModeBtn" class="plan-mode-btn locked" title="Plan Mode (read-only)">
                <span class="lock-icon">✋</span>
            </button>
            <div class="session-picker">
                <button class="session-btn" id="sessionBtn">
                    <span id="sessionLabel">New Session</span>
                    <span>▼</span>
                </button>
                <div class="session-dropdown" id="sessionDropdown">
                    <div class="session-dropdown-header">
                        <span>Sessions</span>
                        <button class="new-session-btn" id="newSessionBtn">+ New</button>
                    </div>
                    <div id="sessionList">
                        <div class="no-sessions">Loading...</div>
                    </div>
                </div>
            </div>
        </div>
        <div class="project-name" id="projectName"></div>
        <h1>Basil <span class="logo">🌿</span></h1>
    </div>

    <div class="chat-container" id="chatContainer">
        <div class="empty-state" id="emptyState">
            <div class="icon">💬</div>
            <p>Start a conversation with Claude</p>
        </div>
    </div>

    <div class="typing-indicator" id="typingIndicator">
        <span></span><span></span><span></span>
    </div>

    <div class="input-container">
        <div class="input-wrapper">
            <div class="scroll-buttons">
                <button class="scroll-btn" id="scrollUpBtn" title="Scroll to top">↑</button>
                <button class="scroll-btn" id="scrollDownBtn" title="Scroll to bottom">↓</button>
            </div>
            <textarea id="messageInput" placeholder="Type your message..." rows="1" autofocus></textarea>
            <button id="sendBtn">
                Send
                <span>→</span>
            </button>
        </div>
    </div>

    <script>
        // Configure marked
        marked.setOptions({
            breaks: true,
            gfm: true
        });

        const chatContainer = document.getElementById('chatContainer');
        const messageInput = document.getElementById('messageInput');
        const sendBtn = document.getElementById('sendBtn');
        const typingIndicator = document.getElementById('typingIndicator');
        const emptyState = document.getElementById('emptyState');
        const sessionBtn = document.getElementById('sessionBtn');
        const sessionDropdown = document.getElementById('sessionDropdown');
        const sessionList = document.getElementById('sessionList');
        const sessionLabel = document.getElementById('sessionLabel');
        const newSessionBtn = document.getElementById('newSessionBtn');

        let currentSession = null;
        let userScrolledUp = false;
        let programmaticScroll = false;
        let planMode = true;  // Default: locked (plan mode)

        // Plan mode toggle
        const planModeBtn = document.getElementById('planModeBtn');

        function updatePlanModeUI() {
            planModeBtn.classList.toggle('locked', planMode);
            planModeBtn.classList.toggle('unlocked', !planMode);
            planModeBtn.querySelector('.lock-icon').textContent = planMode ? '✋' : '⚡';
            planModeBtn.title = planMode ? 'Plan Mode (read-only)' : 'Execute Mode (can make changes)';
        }

        planModeBtn.addEventListener('click', async () => {
            planMode = !planMode;
            updatePlanModeUI();

            // Save mode to session if we have one
            if (currentSession) {
                try {
                    await fetch(`/api/session/${currentSession}/mode`, {
                        method: 'PATCH',
                        headers: { 'Content-Type': 'application/json' },
                        body: JSON.stringify({ plan_mode: planMode })
                    });
                } catch (e) {
                    console.error('Failed to save mode:', e);
                }
            }
        });

        // Connection status polling
        const connectionStatus = document.getElementById('connectionStatus');
        async function checkConnection() {
            try {
                const resp = await fetch('/health', { method: 'GET', signal: AbortSignal.timeout(3000) });
                if (resp.ok) {
                    connectionStatus.className = 'status-circle connected';
                    connectionStatus.title = 'Connected';
                } else {
                    throw new Error('Not OK');
                }
            } catch (e) {
                connectionStatus.className = 'status-circle disconnected';
                connectionStatus.title = 'Disconnected';
            }
        }
        checkConnection();
        setInterval(checkConnection, 3000);  // Check every 3 seconds

        // Load project name
        async function loadProjectName() {
            try {
                const resp = await fetch('/project');
                const data = await resp.json();
                const projectNameEl = document.getElementById('projectName');
                projectNameEl.textContent = data.name;
                if (data.path) {
                    projectNameEl.title = data.path;
                }
            } catch (e) {
                console.error('Failed to load project name:', e);
            }
        }
        loadProjectName();

        // Load last session on startup
        async function loadLastSession() {
            const lastSessionId = localStorage.getItem('lastSessionId');
            if (lastSessionId) {
                try {
                    const resp = await fetch(`/api/session/${lastSessionId}`);
                    if (resp.ok) {
                        await selectSession(lastSessionId);
                    } else {
                        localStorage.removeItem('lastSessionId');
                    }
                } catch (e) {
                    localStorage.removeItem('lastSessionId');
                }
            }
        }

        // Track if user scrolls up manually (ignore our own scrolls)
        chatContainer.addEventListener('scroll', () => {
            if (programmaticScroll) return;
            const threshold = 150;
            const atBottom = chatContainer.scrollHeight - chatContainer.scrollTop - chatContainer.clientHeight < threshold;
            userScrolledUp = !atBottom;
        });

        // Session picker toggle
        sessionBtn.addEventListener('click', async (e) => {
            e.stopPropagation();
            sessionDropdown.classList.toggle('open');
            if (sessionDropdown.classList.contains('open')) {
                await loadSessions();
            }
        });

        document.addEventListener('click', () => {
            sessionDropdown.classList.remove('open');
        });

        sessionDropdown.addEventListener('click', (e) => {
            e.stopPropagation();
        });

        newSessionBtn.addEventListener('click', async () => {
            currentSession = null;
            sessionLabel.textContent = 'New Session';
            clearChat();
            sessionDropdown.classList.remove('open');
        });

        async function loadSessions() {
            try {
                const resp = await fetch('/api/session/list');
                const data = await resp.json();

                if (data.sessions.length === 0) {
                    sessionList.innerHTML = '<div class="no-sessions">No previous sessions</div>';
                    return;
                }

                sessionList.innerHTML = data.sessions.map(s => `
                    <div class="session-item ${s.session_id === currentSession ? 'active' : ''}" data-id="${s.session_id}">
                        <div class="session-info">
                            <div class="session-name" data-id="${s.session_id}">
                                ${s.is_processing ? '<span class="processing-dot"></span>' : ''}
                                ${s.name || formatDate(s.created_at)}
                            </div>
                            <div class="session-dir">${s.working_dir}</div>
                        </div>
                        <div class="session-actions">
                            <button class="session-rename" data-id="${s.session_id}" title="Rename">✏️</button>
                            <button class="session-delete" data-id="${s.session_id}" title="Delete">✕</button>
                        </div>
                    </div>
                `).join('');

                // Add click handlers
                sessionList.querySelectorAll('.session-item').forEach(item => {
                    item.addEventListener('click', (e) => {
                        if (!e.target.classList.contains('session-delete')) {
                            selectSession(item.dataset.id);
                        }
                    });
                });

                sessionList.querySelectorAll('.session-delete').forEach(btn => {
                    btn.addEventListener('click', async (e) => {
                        e.stopPropagation();
                        await deleteSession(btn.dataset.id);
                    });
                });

                // Rename buttons
                sessionList.querySelectorAll('.session-rename').forEach(btn => {
                    btn.addEventListener('click', (e) => {
                        e.stopPropagation();
                        const sessionItem = btn.closest('.session-item');
                        const nameEl = sessionItem.querySelector('.session-name');
                        startRename(nameEl, btn);
                    });
                });
            } catch (err) {
                sessionList.innerHTML = '<div class="no-sessions">Error loading sessions</div>';
            }
        }

        function startRename(nameEl, renameBtn) {
            const sessionId = nameEl.dataset.id;
            const currentName = nameEl.textContent.trim();

            // Hide the pencil button during edit
            if (renameBtn) renameBtn.style.display = 'none';

            const input = document.createElement('input');
            input.type = 'text';
            input.className = 'session-name-input';
            input.value = currentName;

            // Prevent clicks on input from selecting the session
            input.addEventListener('click', (e) => e.stopPropagation());

            nameEl.innerHTML = '';
            nameEl.appendChild(input);
            input.focus();
            input.select();

            const save = async () => {
                const newName = input.value.trim();
                if (newName && newName !== currentName) {
                    try {
                        await fetch(`/api/session/${sessionId}/rename`, {
                            method: 'PATCH',
                            headers: { 'Content-Type': 'application/json' },
                            body: JSON.stringify({ name: newName })
                        });
                        // Update header label if this is current session
                        if (sessionId === currentSession) {
                            sessionLabel.textContent = newName;
                        }
                    } catch (err) {
                        console.error('Rename failed:', err);
                    }
                }
                nameEl.textContent = newName || currentName;
                if (renameBtn) renameBtn.style.display = '';
            };

            input.addEventListener('blur', save);
            input.addEventListener('keypress', (e) => {
                if (e.key === 'Enter') {
                    input.blur();
                }
            });
            input.addEventListener('keydown', (e) => {
                if (e.key === 'Escape') {
                    nameEl.textContent = currentName;
                    if (renameBtn) renameBtn.style.display = '';
                }
            });
        }

        function formatDate(isoDate) {
            try {
                const d = new Date(isoDate);
                return d.toLocaleString();
            } catch {
                return isoDate;
            }
        }

        async function selectSession(sessionId) {
            currentSession = sessionId;
            localStorage.setItem('lastSessionId', sessionId);
            sessionDropdown.classList.remove('open');
            clearChat();

            // Load session history
            try {
                const resp = await fetch(`/api/session/${sessionId}`);
                const data = await resp.json();
                sessionLabel.textContent = data.name || sessionId.substring(0, 8) + '...';

                // Load plan mode from session
                if (data.plan_mode !== undefined) {
                    planMode = data.plan_mode;
                    updatePlanModeUI();
                }

                // Render history
                if (data.messages && data.messages.length > 0) {
                    for (const msg of data.messages) {
                        if (msg.role === 'tool') {
                            // Parse tool message JSON
                            try {
                                const toolData = JSON.parse(msg.content);
                                const el = createMessageElement('assistant');
                                el.classList.add('tool-message');
                                addToolAction(el, toolData.tool, toolData.input || {});
                            } catch(e) {
                                console.error('Failed to parse tool message:', e);
                            }
                        } else {
                            const el = createMessageElement(msg.role);
                            if (msg.role === 'assistant') {
                                renderMarkdown(el, msg.content);
                            } else {
                                el.textContent = msg.content;
                            }
                        }
                    }
                }
            } catch (err) {
                console.error('Failed to load session:', err);
                sessionLabel.textContent = sessionId.substring(0, 8) + '...';
            }
        }

        async function deleteSession(sessionId) {
            if (!confirm('Delete this session?')) return;

            try {
                await fetch(`/api/session/${sessionId}`, { method: 'DELETE' });
                if (currentSession === sessionId) {
                    currentSession = null;
                    sessionLabel.textContent = 'New Session';
                    clearChat();
                }
                await loadSessions();
            } catch (err) {
                alert('Failed to delete session');
            }
        }

        function clearChat() {
            chatContainer.innerHTML = `
                <div class="empty-state" id="emptyState">
                    <div class="icon">💬</div>
                    <p>Start a conversation with Claude</p>
                </div>
            `;
        }

        function createMessageElement(type) {
            if (emptyState) emptyState.remove();

            const msg = document.createElement('div');
            msg.className = `message ${type}`;
            chatContainer.appendChild(msg);
            scrollToBottom();
            return msg;
        }

        function renderMarkdown(element, text) {
            element.innerHTML = marked.parse(text);
            // Apply syntax highlighting to code blocks
            element.querySelectorAll('pre code').forEach((block) => {
                if (typeof hljs !== 'undefined') {
                    hljs.highlightElement(block);
                }
            });
            // Scroll after everything is rendered
            scrollToBottom();
        }

        function scrollToBottom(force = false) {
            // Double rAF to ensure DOM is fully laid out
            requestAnimationFrame(() => {
                requestAnimationFrame(() => {
                    if (force || !userScrolledUp) {
                        programmaticScroll = true;
                        chatContainer.scrollTop = chatContainer.scrollHeight;
                        // Reset flag after scroll event fires
                        setTimeout(() => { programmaticScroll = false; }, 50);
                    }
                });
            });
        }

        function addToolAction(element, toolName, toolInput) {
            let actionsDiv = element.querySelector('.tool-actions');
            if (!actionsDiv) {
                actionsDiv = document.createElement('div');
                actionsDiv.className = 'tool-actions';
                element.insertBefore(actionsDiv, element.firstChild);
            }

            // Create action log entry
            const action = document.createElement('div');
            action.className = 'tool-action';

            const icon = getToolIcon(toolName);
            const detail = formatToolInput(toolName, toolInput);
            action.innerHTML = `<span class="tool-icon">${icon}</span><span class="tool-name">${toolName}</span><span class="tool-detail">${detail}</span>`;

            actionsDiv.appendChild(action);
            scrollToBottom();
        }

        function getToolIcon(toolName) {
            const icons = {
                // File operations
                'Read': '📄',
                'Edit': '✏️',
                'Write': '📝',
                'Glob': '📁',
                'Grep': '🔎',
                // Shell
                'Bash': '⚡',
                // Web
                'WebFetch': '📡',
                'WebSearch': '🌍',
                // Agents & Tasks
                'Task': '🤖',
                'TodoRead': '📋',
                'TodoWrite': '✅',
                // User interaction
                'AskUserQuestion': '❓',
                // Notebooks
                'NotebookEdit': '📓',
                // Multi-file
                'MultiEdit': '📑',
            };
            return icons[toolName] || '🔧';
        }

        function formatToolInput(toolName, input) {
            // Format tool input nicely based on tool type
            if (toolName === 'Read' && input.file_path) {
                return input.file_path;
            }
            if (toolName === 'Bash' && input.command) {
                return input.command;
            }
            if (toolName === 'Edit' && input.file_path) {
                return input.file_path;
            }
            if (toolName === 'Write' && input.file_path) {
                return input.file_path;
            }
            if (toolName === 'Glob' && input.pattern) {
                return input.pattern;
            }
            if (toolName === 'Grep' && input.pattern) {
                return input.pattern + (input.path ? ' in ' + input.path : '');
            }
            // Fallback: show JSON
            return JSON.stringify(input, null, 2);
        }

        async function ensureSession() {
            if (!currentSession) {
                const resp = await fetch('/api/session/new', {
                    method: 'POST',
                    headers: { 'Content-Type': 'application/json' },
                    body: JSON.stringify({ working_dir: '/workspace' })
                });
                const data = await resp.json();
                currentSession = data.session_id;
                localStorage.setItem('lastSessionId', currentSession);
                sessionLabel.textContent = data.name || currentSession.substring(0, 8) + '...';
            }
            return currentSession;
        }

        async function sendMessage() {
            const text = messageInput.value.trim();
            if (!text) return;

            messageInput.value = '';
            messageInput.style.height = 'auto';  // Reset textarea height
            userScrolledUp = false;  // Reset scroll tracking on new message
            const userMsg = createMessageElement('user');
            userMsg.textContent = text;

            sendBtn.disabled = true;
            typingIndicator.classList.add('active');
            chatContainer.appendChild(typingIndicator);
            scrollToBottom();

            try {
                const sessionId = await ensureSession();

                // Start the chat
                await fetch('/api/chat', {
                    method: 'POST',
                    headers: {
                        'Content-Type': 'application/json',
                        'X-Session': sessionId
                    },
                    body: JSON.stringify({ text: text, plan_mode: planMode })
                });

                // Poll for responses - each text block becomes its own bubble
                let currentMsg = null;
                let lastText = '';

                let more = true;
                while (more) {
                    const resp = await fetch(`/api/chat/next?timeout=30`, {
                        headers: { 'X-Session': sessionId }
                    });
                    const block = await resp.json();

                    if (block.type === 'text' && block.content) {
                        // Check if this is new/different text (not just the same text growing)
                        if (block.content !== lastText) {
                            // Create new bubble for this text
                            currentMsg = createMessageElement('assistant');
                            renderMarkdown(currentMsg, block.content);
                            lastText = block.content;
                        }
                    } else if (block.type === 'tool') {
                        const toolName = block.tool || 'tool';
                        const toolInput = block.input || {};
                        // Create tool bubble
                        const toolMsg = createMessageElement('assistant');
                        toolMsg.classList.add('tool-message');
                        addToolAction(toolMsg, toolName, toolInput);
                        currentMsg = toolMsg;
                    } else if (block.type === 'error') {
                        const errMsg = createMessageElement('system');
                        errMsg.textContent = block.content;
                    }

                    more = block.more;
                }

                // Done - hide typing and deactivate tools
                typingIndicator.classList.remove('active');

            } catch (err) {
                typingIndicator.classList.remove('active');
                const errMsg = createMessageElement('system');
                errMsg.textContent = 'Error: ' + err.message;
            }

            sendBtn.disabled = false;
            messageInput.focus();
        }

        sendBtn.addEventListener('click', sendMessage);

        // Auto-resize textarea
        function autoResize() {
            messageInput.style.height = 'auto';
            messageInput.style.height = Math.min(messageInput.scrollHeight, 200) + 'px';
            // Show scrollbar if at max height
            messageInput.style.overflowY = messageInput.scrollHeight > 200 ? 'auto' : 'hidden';
        }

        messageInput.addEventListener('input', autoResize);

        messageInput.addEventListener('keydown', (e) => {
            if (e.key === 'Enter' && !e.shiftKey) {
                e.preventDefault();
                sendMessage();
            }
            // Shift+Enter inserts newline (default behavior)
        });

        // Scroll buttons
        document.getElementById('scrollUpBtn').addEventListener('click', () => {
            chatContainer.scrollTo({ top: 0, behavior: 'smooth' });
            userScrolledUp = true;
        });
        document.getElementById('scrollDownBtn').addEventListener('click', () => {
            chatContainer.scrollTo({ top: chatContainer.scrollHeight, behavior: 'smooth' });
            userScrolledUp = false;
        });

        // Initialize
        updatePlanModeUI();  // Set initial button state
        loadLastSession();
    </script>
</body>
</html>
"""


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
            return CHAT_HTML

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
