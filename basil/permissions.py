"""Permission request handling."""

import asyncio
import time
from dataclasses import dataclass, field
from typing import Any


@dataclass
class PermissionRequest:
    """A pending permission request."""
    permission_id: str
    tool_name: str
    description: str
    tool_input: dict
    claude_session_id: str
    created_at: float = field(default_factory=time.time)

    # Decision state
    decided: bool = False
    decision: str | None = None  # "allow" or "deny"
    message: str | None = None

    # Event for waiting
    _event: asyncio.Event = field(default_factory=asyncio.Event, repr=False)

    def set_decision(self, allow: bool, message: str | None = None):
        """Set the decision for this permission request."""
        self.decided = True
        self.decision = "allow" if allow else "deny"
        self.message = message
        self._event.set()

    async def wait_for_decision(self, timeout: float = 120) -> bool:
        """Wait for a decision. Returns True if decided, False if timed out."""
        try:
            await asyncio.wait_for(self._event.wait(), timeout=timeout)
            return True
        except asyncio.TimeoutError:
            return False


class PermissionStore:
    """Store for pending permission requests."""

    def __init__(self, ttl: int = 300):
        self._requests: dict[str, PermissionRequest] = {}
        self._ttl = ttl  # Time to live in seconds

    def add(self, request: PermissionRequest) -> None:
        """Add a permission request."""
        self._cleanup()
        self._requests[request.permission_id] = request

    def get(self, permission_id: str) -> PermissionRequest | None:
        """Get a permission request by ID."""
        self._cleanup()
        return self._requests.get(permission_id)

    def remove(self, permission_id: str) -> None:
        """Remove a permission request."""
        self._requests.pop(permission_id, None)

    def get_pending_for_session(self, claude_session_id: str) -> PermissionRequest | None:
        """Get the first pending permission for a Claude session."""
        self._cleanup()
        for req in self._requests.values():
            if req.claude_session_id == claude_session_id and not req.decided:
                return req
        return None

    def _cleanup(self) -> None:
        """Remove expired requests."""
        now = time.time()
        expired = [
            pid for pid, req in self._requests.items()
            if now - req.created_at > self._ttl
        ]
        for pid in expired:
            del self._requests[pid]


# Global permission store
permissions = PermissionStore()
