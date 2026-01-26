#!/bin/bash
# Basil System Files Installer
# Installs hooks and shows what to add to settings.json

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
CLAUDE_DIR="$HOME/.claude"
HOOKS_DIR="$CLAUDE_DIR/hooks"

echo "=== Basil System Files Installer ==="
echo ""

# Create directories
mkdir -p "$HOOKS_DIR"

# Copy hook
echo "Installing hook..."
cp "$SCRIPT_DIR/claude-hooks/basil_permission.py" "$HOOKS_DIR/"
chmod +x "$HOOKS_DIR/basil_permission.py"
echo "  ✓ Installed: $HOOKS_DIR/basil_permission.py"

# Show settings instructions
echo ""
echo "=== Manual Step Required ==="
echo ""
echo "Add this to your ~/.claude/settings.json inside the 'hooks' object:"
echo ""
cat "$SCRIPT_DIR/claude-settings/settings-addition.json" | grep -v "_comment" | grep -v "_install"
echo ""
echo "Example: If your settings.json has:"
echo '  "hooks": {'
echo '    "Stop": [...]'
echo '  }'
echo ""
echo "Change it to:"
echo '  "hooks": {'
echo '    "PermissionRequest": [...],  <-- ADD THIS'
echo '    "Stop": [...]'
echo '  }'
echo ""
echo "=== Done ==="
