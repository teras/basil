"""Session management for chat conversations."""

import json
import uuid
import asyncio
from dataclasses import dataclass, field, asdict
from datetime import datetime
from pathlib import Path
from typing import Any

from .config import get_settings


@dataclass
class Message:
    """A single message in the conversation."""
    role: str  # "user" or "assistant"
    content: str
    timestamp: str = field(default_factory=lambda: datetime.now().isoformat())


@dataclass
class ResponseBlock:
    """A block of response from Claude."""
    block_id: int
    content: str
    block_type: str = "text"  # "text", "code", "permission", "error"
    more: bool = False
    metadata: dict = field(default_factory=dict)

    def to_dict(self) -> dict:
        return {
            "block_id": self.block_id,
            "content": self.content,
            "type": self.block_type,
            "more": self.more,
            **self.metadata
        }


@dataclass
class Session:
    """A chat session with Claude."""
    session_id: str
    working_dir: str
    created_at: str = field(default_factory=lambda: datetime.now().isoformat())
    claude_session_id: str | None = None  # Claude's internal session ID for --continue

    # Runtime state (not persisted)
    response_queue: asyncio.Queue = field(default_factory=asyncio.Queue, repr=False)
    is_processing: bool = False
    current_block_id: int = 0
    pending_permission: dict | None = None

    def next_block_id(self) -> int:
        self.current_block_id += 1
        return self.current_block_id

    def to_dict(self) -> dict:
        """Serialize for storage (excludes runtime state)."""
        return {
            "session_id": self.session_id,
            "working_dir": self.working_dir,
            "created_at": self.created_at,
            "claude_session_id": self.claude_session_id,
        }

    @classmethod
    def from_dict(cls, data: dict) -> "Session":
        """Deserialize from storage."""
        return cls(
            session_id=data["session_id"],
            working_dir=data["working_dir"],
            created_at=data.get("created_at", datetime.now().isoformat()),
            claude_session_id=data.get("claude_session_id"),
        )


class SessionManager:
    """Manages chat sessions."""

    def __init__(self):
        self._sessions: dict[str, Session] = {}

    def create_session(self, working_dir: str | None = None) -> Session:
        """Create a new session."""
        settings = get_settings()
        session_id = uuid.uuid4().hex[:12]

        if working_dir is None:
            working_dir = str(settings.default_working_dir)

        session = Session(
            session_id=session_id,
            working_dir=working_dir,
        )
        self._sessions[session_id] = session
        self._save_session(session)
        return session

    def get_session(self, session_id: str) -> Session | None:
        """Get a session by ID."""
        # Check memory first
        if session_id in self._sessions:
            return self._sessions[session_id]

        # Try to load from disk
        session = self._load_session(session_id)
        if session:
            self._sessions[session_id] = session
        return session

    def list_sessions(self) -> list[dict]:
        """List all sessions."""
        settings = get_settings()
        sessions = []

        for path in settings.session_dir.glob("*.json"):
            try:
                with open(path) as f:
                    data = json.load(f)
                    sessions.append({
                        "session_id": data["session_id"],
                        "working_dir": data["working_dir"],
                        "created_at": data.get("created_at", "unknown"),
                    })
            except Exception:
                continue

        # Sort by created_at descending
        sessions.sort(key=lambda x: x.get("created_at", ""), reverse=True)
        return sessions

    def update_session(self, session: Session) -> None:
        """Save session state."""
        self._save_session(session)

    def delete_session(self, session_id: str) -> bool:
        """Delete a session."""
        settings = get_settings()
        path = settings.session_dir / f"{session_id}.json"

        if session_id in self._sessions:
            del self._sessions[session_id]

        if path.exists():
            path.unlink()
            return True
        return False

    def _save_session(self, session: Session) -> None:
        """Save session to disk."""
        settings = get_settings()
        path = settings.session_dir / f"{session.session_id}.json"
        with open(path, "w") as f:
            json.dump(session.to_dict(), f, indent=2)

    def _load_session(self, session_id: str) -> Session | None:
        """Load session from disk."""
        settings = get_settings()
        path = settings.session_dir / f"{session_id}.json"

        if not path.exists():
            return None

        try:
            with open(path) as f:
                data = json.load(f)
            return Session.from_dict(data)
        except Exception as e:
            print(f"Failed to load session {session_id}: {e}")
            return None


# Global session manager
sessions = SessionManager()
