# Basil Development Documentation

## Τι είναι το Basil

Basil είναι ένα clean HTTP API bridge για το Claude Code CLI. Επιτρέπει:
- Αποστολή μηνυμάτων στο Claude μέσω HTTP requests
- Λήψη streaming responses σε blocks
- Session management (multiple named sessions)
- Permission handling μέσω HTTP callbacks

## Γιατί το φτιάχνουμε

Ξεκινήσαμε από ανάλυση του [claudecode-telegram](https://github.com/hanxiao/claudecode-telegram) που είχε σοβαρά security issues:
- Χρήση `--dangerously-skip-permissions`
- Καμία authentication
- tmux hack (fragile)

Αποφασίσαμε να φτιάξουμε κάτι:
- **Clean**: HTTP API αντί για Telegram-specific
- **Secure**: Proper permission handling
- **Flexible**: Μπορεί να συνδεθεί web UI, terminal, mobile app

---

## Αρχιτεκτονική

```
┌─────────────────────────────────────────────┐
│              Clients                         │
│  (curl, web UI, mobile app, telegram bot)   │
└─────────────────────────────────────────────┘
                     │
                     ▼
┌─────────────────────────────────────────────┐
│           Basil Server (FastAPI)            │
│  - POST /session/new                        │
│  - POST /chat  (send message)               │
│  - GET  /chat/next (get response block)     │
│  - POST /permission/{id}/decide             │
│  Port: 8765                                 │
└─────────────────────────────────────────────┘
                     │
                     ▼
┌─────────────────────────────────────────────┐
│        Claude Code CLI (claude -p)          │
│  - Non-interactive mode                     │
│  - --output-format stream-json --verbose    │
│  - Session continuity via --resume          │
└─────────────────────────────────────────────┘
                     │
                     ▼
┌─────────────────────────────────────────────┐
│    PermissionRequest Hook (basil_permission.py)
│  - Intercepts permission requests           │
│  - Posts to Basil server                    │
│  - Polls for user decision                  │
│  - Returns allow/deny to Claude             │
└─────────────────────────────────────────────┘
```

---

## Πού έχουμε φτάσει

### ✅ Completed

1. **Project skeleton**
   - `/home/teras/experiments/basil/`
   - Python package με FastAPI

2. **Core API**
   - `POST /session/new` - Create session
   - `GET /session/list` - List sessions
   - `POST /chat` - Send message
   - `GET /chat/next` - Get response block
   - `POST /chat/stop` - Stop Claude

3. **Claude integration**
   - `basil/claude.py` - Runs `claude -p --output-format stream-json --verbose`
   - Streaming responses
   - Session continuity via `--resume`

4. **CLI client**
   - `./basil-cli` - Bash script for testing
   - Commands: new, list, send, chat, status, stop

5. **Permission handling endpoints**
   - `POST /hook/permission` - Hook posts request
   - `GET /hook/permission/{id}/decision` - Hook polls for decision
   - `POST /permission/{id}/decide` - User decides

6. **Hook script created**
   - `~/.claude/hooks/basil_permission.py`
   - Added to `~/.claude/settings.json`

### ❌ Challenges

**Πρόβλημα: Το PermissionRequest hook δεν καλείται**

- Ρυθμίσαμε το hook στο `settings.json`
- Αλλά δεν γράφει στο log file `/tmp/basil_hook.log`
- Πιθανές αιτίες:
  1. ~~Λάθος output format~~ (διορθώσαμε `decision` → `behavior`)
  2. Το hook δεν τρέχει καθόλου σε `-p` mode με default permissions
  3. Πρέπει να χρησιμοποιήσουμε `--permission-mode` flag

---

## Τι μένει

### 1. Fix permission hook (CURRENT)
- Verify hook is being called
- Test with `--permission-mode default`
- May need different approach

### 2. Web UI
- Simple HTML/JS interface
- WebSocket or polling for responses

### 3. Session switching
- `/switch` command in CLI

### 4. Documentation
- README με setup instructions
- Example configurations

---

## Files Modified Outside Project

```
~/.claude/settings.json
  - Added PermissionRequest hook

~/.claude/hooks/basil_permission.py
  - New hook script
```

---

## Πώς να τρέξεις

### 1. Start server

```bash
cd /home/teras/experiments/basil
source .venv/bin/activate
PORT=8765 python -m basil.main
```

### 2. Test με CLI

```bash
./basil-cli new /path/to/project
./basil-cli send "Hello Claude"
./basil-cli status
./basil-cli list
```

### 3. Test με curl

```bash
# Create session
SESSION=$(curl -s -X POST http://localhost:8765/session/new \
  -H "Content-Type: application/json" \
  -d '{"working_dir": "/tmp"}' | jq -r '.session_id')

# Send message
echo '{"text":"List files"}' > /tmp/msg.json
curl -s -X POST http://localhost:8765/chat \
  -H "Content-Type: application/json" \
  -H "X-Session: $SESSION" \
  --data-binary @/tmp/msg.json

# Get response blocks
curl -s "http://localhost:8765/chat/next?timeout=30" \
  -H "X-Session: $SESSION"
```

---

## Επόμενο Test (Permission Hook)

### Βήμα 1: Verify hook output format
```bash
# Check if "behavior" instead of "decision" fixes it
cat ~/.claude/hooks/basil_permission.py | grep "behavior"
```

### Βήμα 2: Test hook directly
```bash
echo '{"tool_name":"Write","tool_input":{"file_path":"/tmp/test.txt"}}' | \
  python3 ~/.claude/hooks/basil_permission.py
# Should see output in /tmp/basil_hook.log
```

### Βήμα 3: Test με Claude
```bash
rm /tmp/basil_hook.log
./basil-cli new /tmp
./basil-cli send "Create file /tmp/test.txt with content 'hello'"
# Wait 5 seconds
cat /tmp/basil_hook.log
```

### Βήμα 4: If hook not called, try permission-mode
```bash
# May need to modify claude.py to add:
# --permission-mode default
```

---

## Τελικό Test (Full Flow)

1. Start Basil server
2. Create session
3. Send message that requires permission (e.g., "Create a file")
4. See permission prompt in CLI
5. Type 'y' to allow
6. See Claude complete the action
7. Verify file was created

---

## Project Files

```
/home/teras/experiments/basil/
│
├── basil/                        # [PROJECT] Python package
│   ├── __init__.py              # Package init
│   ├── config.py                # Pydantic settings
│   ├── sessions.py              # Session management
│   ├── permissions.py           # Permission request store
│   ├── claude.py                # Claude CLI wrapper
│   └── main.py                  # FastAPI app
│
├── system-files/                 # [SYSTEM] Files που πάνε στο ~/.claude/
│   ├── install.sh               # Installer script
│   ├── claude-hooks/
│   │   └── basil_permission.py  # → ~/.claude/hooks/basil_permission.py
│   └── claude-settings/
│       └── settings-addition.json  # Τι να προσθέσεις στο settings.json
│
├── hooks/                        # [TEMPLATE] Backup copy
│   └── permission_request.py
│
├── basil-cli                     # CLI client script
├── pyproject.toml               # Python package config
├── .env.example                 # Example environment
└── DEVELOPMENT.md               # This file
```

### Επεξήγηση:
- **[PROJECT]**: Αρχεία που τρέχουν μόνο στο Basil project
- **[SYSTEM]**: Αρχεία που πρέπει να εγκατασταθούν στο `~/.claude/` (επηρεάζουν ΌΛΕΣ τις Claude sessions)
- **[TEMPLATE]**: Backup copies για reference

---

## Clean Setup (νέο μηχάνημα)

```bash
# 1. Clone/copy project
cd /path/to/experiments
# Copy basil folder

# 2. Create venv
cd basil
python -m venv .venv
source .venv/bin/activate
pip install -e .

# 3. Install system files (AFFECTS ALL CLAUDE SESSIONS!)
./system-files/install.sh
# Then manually edit ~/.claude/settings.json as instructed

# 4. Start server
PORT=8765 python -m basil.main

# 5. Test
./basil-cli new $(pwd)
./basil-cli send "Hello"
```

### ⚠️ ΣΗΜΑΝΤΙΚΟ: System Files

Τα αρχεία στο `system-files/` εγκαθίστανται στο `~/.claude/` και **επηρεάζουν
ΟΛΕΣ τις Claude Code sessions**, όχι μόνο το Basil!

Αν θες να τρέξεις Claude κανονικά χωρίς Basil, πρέπει να κάνεις rollback
(δες παρακάτω την ενότητα "Αλλαγές στο σύστημα").

---

## Known Issues

1. **Permission hook not triggering** - Under investigation
2. **Session not persisting claude_session_id** - Need to verify
3. **No web UI yet** - Planned for later phase

---

## Current Status (PAUSED)

**Πρόβλημα:** Το PermissionRequest hook δεν καλείται καθόλου σε `-p` mode.

**Τι δοκιμάσαμε:**
- Προσθέσαμε hook στο `~/.claude/settings.json`
- Δημιουργήσαμε `~/.claude/hooks/basil_permission.py` με logging
- Διορθώσαμε output format (`decision` → `behavior`)
- Test με `./basil-cli send "Create file..."` - Hook log EMPTY

**Πιθανές λύσεις να δοκιμαστούν:**
1. Χρήση `--permission-mode` flag στο `claude -p`
2. Χρήση `PreToolUse` hook αντί για `PermissionRequest`
3. Χρήση `--allowedTools` για explicit control
4. Έλεγχος αν hooks δουλεύουν καθόλου σε headless mode

**Επόμενο βήμα:**
```bash
# Δοκίμασε αυτό για να δεις αν hooks τρέχουν:
echo "hello" | claude -p --verbose 2>&1 | grep -i hook

# Ή δοκίμασε με explicit permission mode:
# Modify basil/claude.py to add: "--permission-mode", "default"
```

---

## Αλλαγές στο σύστημα (για rollback)

### Files created/modified outside project:

```
~/.claude/settings.json
  ADDED (στην αρχή του "hooks" object):
    "PermissionRequest": [
      {
        "matcher": "*",
        "hooks": [
          {
            "type": "command",
            "command": "~/.claude/hooks/basil_permission.py"
          }
        ]
      }
    ],

~/.claude/hooks/basil_permission.py
  NEW FILE - permission hook script
```

### To rollback:
```bash
# Remove hook from settings.json (edit manually, remove PermissionRequest section)
# Delete hook file:
rm ~/.claude/hooks/basil_permission.py
```

---

## Working Features (tested OK)

```bash
# These all work:
./basil-cli new /path          # Create session ✅
./basil-cli send "Hello"       # Send to Claude ✅
./basil-cli list               # List sessions ✅
./basil-cli status             # Show status ✅

# Claude responds with streaming blocks ✅
# Session continuity works ✅
```

---

*Last updated: 2026-01-26 19:30*
*Status: PAUSED - Permission hook investigation*
