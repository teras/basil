# 🌿 Basil

**🌿 Basil: Docker-isolated Claude Code with a web UI — full autonomy, zero risk**

## 🤔 Why Basil

### 🔒 No more permission prompts

Claude Code is powerful, but on your bare machine every file write, every shell command needs your approval. Basil runs Claude inside a Docker container — nothing can escape to your host system. This means `--dangerously-skip-permissions` is actually safe, and Claude can work autonomously without asking you to approve every single action.

Need a package installed? Claude requests it through Basil, you approve once, and the container rebuilds with it baked in — no isolation lost. Need access to a folder on your machine? Same flow: approve the mount, and it appears inside the container read-only.

### 🌍 Access your machine from anywhere

Basil exposes Claude Code over HTTP with a built-in web UI. No SSH needed. Open a browser from your phone, tablet, or another computer and you have full access to Claude working on your project. The API also means you can integrate Claude into scripts, CI pipelines, or other tools.

## 🚀 Quick Start

### Prerequisites

- 🦀 [Rust](https://rustup.rs/) (for building)
- 🐳 [Docker](https://docs.docker.com/get-docker/) (running daemon)
- 🔑 Anthropic API key configured (`claude login`)

### Build & Run

```bash
cargo build --release
./target/release/basil /path/to/your/project
```

Basil will:

1. 📦 Build a Docker base image (first run only, includes Node.js 22, Python 3, common tools)
2. 🐳 Start an isolated container for your project
3. 🌐 Open a web server — visit the URL printed in your terminal

### Examples

```bash
# Run on current directory
basil .

# Custom port
basil -p 8080 /path/to/project

# API-only (no web UI)
basil --no-ui /path/to/project

# Debug logging
basil -d /path/to/project
```

## ⚙️ How It Works

Each project gets its own Docker container. Your project directory is mounted at `/workspace`. Claude Code runs inside with full permissions — but those permissions are contained to the container.

When Claude needs something outside the sandbox:

- 📦 **Install packages** — Claude requests Dockerfile commands (apt, pip, npm, cargo, anything). You approve in the UI, the container rebuilds with the packages baked into the image, and Claude automatically resumes where it left off.
- 📂 **Access host directories** — Claude requests a mount to a folder on your machine. You approve, the directory appears inside the container, and Claude continues working.

Both flows require explicit user approval. Both survive container restarts. Neither breaks isolation.

## 🖥️ Web UI

The built-in interface provides:

- 💬 Real-time chat with Claude Code
- ✋⚡ Plan mode (safe, default) and Execute mode toggle
- 📋 Session management — multiple sessions per project with full history
- 🎨 Markdown rendering with syntax highlighting
- ✅ Approval dialogs for package installs and directory mounts

## 🛠️ CLI Options

```
basil [OPTIONS] [PATH]

Arguments:
  [PATH]              Project directory [default: .]

Options:
  -p, --port <PORT>   Port (default: auto-assigned 8100-8199)
      --no-ui         Disable web UI
  -d, --debug         Enable debug logging
  -h, --help          Print help
  -V, --version       Print version
```

## 📄 License

AGPL-3.0-or-later. See [LICENSE](LICENSE).
