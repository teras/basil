"""Configuration management using pydantic-settings."""

from pathlib import Path
from pydantic_settings import BaseSettings, SettingsConfigDict


class Settings(BaseSettings):
    """Application settings loaded from environment variables."""

    model_config = SettingsConfigDict(
        env_file=".env",
        env_file_encoding="utf-8",
        extra="ignore",
    )

    # Server
    host: str = "127.0.0.1"
    port: int = 8035

    # Claude
    default_working_dir: Path = Path.home()

    # Sessions - stored in ~/.claude/sessions (mapped to PROJECT/.basil/sessions in Docker)
    session_dir: Path = Path.home() / ".claude" / "sessions"

    # UI
    serve_ui: bool = False

    # Project
    project_name: str = "project"
    project_path: str = ""  # Full path on host (for display purposes)


# Global settings instance
_settings: Settings | None = None


def get_settings() -> Settings:
    """Get or create the settings instance."""
    global _settings
    if _settings is None:
        _settings = Settings()
        # Ensure session directory exists
        _settings.session_dir.mkdir(parents=True, exist_ok=True)
    return _settings
