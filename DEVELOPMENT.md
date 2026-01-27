# Basil

## What is Basil

Basil is an HTTP API bridge for Claude Code CLI, running inside Docker for security and isolation.

**Why Docker?**
- **Security**: Claude Code runs with `--dangerously-skip-permissions`, allowing unrestricted file and command access. Docker containers isolate this to a specific project directory only.
- **Isolation**: Each project gets its own container. No cross-project access, no host system exposure.
- **Reproducibility**: Consistent environment across machines. No dependency conflicts.
- **Easy cleanup**: Stop the container and everything is gone.

**Features:**
- HTTP API for sending messages and receiving streaming responses
- Session management (multiple named sessions)
- Optional Web UI for browser-based chat

## Architecture

```
┌─────────────────────────────────────────────┐
│              Clients                         │
│  (Web UI, curl, mobile app, telegram bot)   │
└─────────────────────────────────────────────┘
                     │
                     ▼
┌─────────────────────────────────────────────┐
│           Basil Server (FastAPI)            │
│  - GET  /              (Web UI, if enabled) │
│  - POST /              (Simple chat)        │
│  - POST /api/session/new                    │
│  - POST /api/chat      (send message)       │
│  - GET  /api/chat/next (get response block) │
│  Port: 8035 (configurable)                  │
└─────────────────────────────────────────────┘
                     │
                     ▼
┌─────────────────────────────────────────────┐
│        Claude Code CLI (claude -p)          │
│  - Non-interactive mode                     │
│  - --output-format stream-json --verbose    │
│  - --dangerously-skip-permissions           │
│  - Session continuity via --resume          │
└─────────────────────────────────────────────┘
```

## Running

### With Docker

```bash
basil build         # Build image (once)

cd /path/to/project
basil start         # API only
basil start --ui    # with Web UI
basil stop
```

First run auto-initializes credentials from `~/.claude/`.

### Standalone (without Docker)

```bash
cd /home/teras/experiments/basil

# API only
HOST=0.0.0.0 PORT=8035 python -m src.main

# API + Web UI
HOST=0.0.0.0 PORT=8035 SERVE_UI=1 python -m src.main
```

## API Endpoints

### Simple (Root)

- `GET /` - Web UI
- `POST /` - Simple sync chat: `{"prompt": "Hello"}` → `{"response": "..."}`
- `GET /health` - Health check

### Sessions

- `POST /api/session/new` - Create session
- `GET /api/session/list` - List sessions
- `GET /api/session/{id}` - Get session info
- `PATCH /api/session/{id}/rename` - Rename session
- `DELETE /api/session/{id}` - Delete session

### Chat (Streaming)

- `POST /api/chat` - Start message (requires `X-Session` header)
- `GET /api/chat/next` - Get next response block (polling)
- `POST /api/chat/stop` - Stop current processing

## Project Structure

```
basil/
├── basil              # Launcher script
├── src/
│   ├── __init__.py
│   ├── config.py      # Settings (HOST, PORT, SERVE_UI)
│   ├── sessions.py    # Session management
│   ├── claude.py      # Claude CLI wrapper
│   └── main.py        # FastAPI app + Web UI
├── pyproject.toml
└── DEVELOPMENT.md
```

## Storage

**Project state:** `PROJECT/.basil/` - Claude credentials, settings, and sessions (gitignored)

**Sessions:** `PROJECT/.basil/sessions/` - Chat sessions as JSON files (per-project)

---

*Last updated: 2026-01-27*
