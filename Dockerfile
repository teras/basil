FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive
ENV NODE_MAJOR=22

# Basic tools + development essentials
RUN apt-get update && apt-get install -y \
    curl wget git sudo \
    build-essential cmake pkg-config \
    python3 python3-pip python3-venv \
    ripgrep fd-find \
    jq tree htop vim nano \
    unzip zip tar \
    openssh-client ca-certificates \
    gnupg \
    && rm -rf /var/lib/apt/lists/*

# Install Node.js 22
RUN curl -fsSL https://deb.nodesource.com/setup_22.x | bash - \
    && apt-get install -y nodejs \
    && rm -rf /var/lib/apt/lists/*

# Install Claude Code globally
RUN npm install -g @anthropic-ai/claude-code

# Install Basil web dependencies
RUN pip3 install --break-system-packages fastapi uvicorn[standard] pydantic-settings

# Create workspace and home directories
RUN mkdir -p /workspace /home/claude/.claude \
    && chmod 777 /home/claude /home/claude/.claude

# Copy Basil source
COPY src /opt/basil/src

WORKDIR /workspace
CMD ["bash"]
