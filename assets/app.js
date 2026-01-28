// Configure marked
marked.setOptions({
    breaks: true,
    gfm: true
});

const chatContainer = document.getElementById('chatContainer');
const messageInput = document.getElementById('messageInput');
const sendBtn = document.getElementById('sendBtn');
const typingIndicator = document.getElementById('typingIndicator');
// Note: emptyState is recreated by clearChat(), so always query DOM when removing
const sessionBtn = document.getElementById('sessionBtn');
const sessionDropdown = document.getElementById('sessionDropdown');
const sessionList = document.getElementById('sessionList');
const sessionLabel = document.getElementById('sessionLabel');
const newSessionBtn = document.getElementById('newSessionBtn');

let currentSession = null;
let userScrolledUp = false;
let programmaticScroll = false;
let planMode = true;  // Default: locked (plan mode)
let isProcessing = false;

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
        document.title = data.name + ' - Basil';
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
    planMode = true;           // Reset to plan mode
    updatePlanModeUI();        // Update UI
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
            let historyToolGroup = null;
            for (const msg of data.messages) {
                if (msg.role === 'tool') {
                    // Parse tool message JSON
                    try {
                        const toolData = JSON.parse(msg.content);
                        // Group consecutive tools
                        if (!historyToolGroup) {
                            historyToolGroup = document.createElement('div');
                            historyToolGroup.className = 'tool-group';
                            chatContainer.appendChild(historyToolGroup);
                            document.getElementById('emptyState')?.remove();
                        }
                        addToolChip(historyToolGroup, toolData.tool, toolData.input || {});
                    } catch(e) {
                        console.error('Failed to parse tool message:', e);
                    }
                } else {
                    // Non-tool message breaks tool group
                    historyToolGroup = null;
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
    document.getElementById('emptyState')?.remove();

    const msg = document.createElement('div');
    msg.className = `message ${type}`;

    // Insert before typing indicator if it's active, otherwise append
    if (typingIndicator.classList.contains('active')) {
        chatContainer.insertBefore(msg, typingIndicator);
    } else {
        chatContainer.appendChild(msg);
    }
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

function addToolChip(container, toolName, toolInput) {
    const chip = document.createElement('div');
    chip.className = 'tool-chip';

    const icon = getToolIcon(toolName);
    const detail = formatToolInput(toolName, toolInput);
    chip.innerHTML = `<span class="tool-icon">${icon}</span><span class="tool-name">${toolName}</span><span class="tool-detail">${detail}</span>`;

    container.appendChild(chip);
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

    isProcessing = true;
    setButtonMode(true);
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
        let currentToolGroup = null;
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
                    // Text breaks the tool group
                    currentToolGroup = null;
                    // Create new bubble for this text
                    currentMsg = createMessageElement('assistant');
                    renderMarkdown(currentMsg, block.content);
                    lastText = block.content;
                }
            } else if (block.type === 'tool') {
                const toolName = block.tool || 'tool';
                const toolInput = block.input || {};
                // Add to current tool group or create new one
                if (!currentToolGroup) {
                    currentToolGroup = document.createElement('div');
                    currentToolGroup.className = 'tool-group';
                    if (typingIndicator.classList.contains('active')) {
                        chatContainer.insertBefore(currentToolGroup, typingIndicator);
                    } else {
                        chatContainer.appendChild(currentToolGroup);
                    }
                    document.getElementById('emptyState')?.remove();
                }
                addToolChip(currentToolGroup, toolName, toolInput);
                currentMsg = currentToolGroup;
            } else if (block.type === 'error') {
                currentToolGroup = null;
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

    isProcessing = false;
    setButtonMode(false);
    messageInput.focus();
}

function setButtonMode(stopMode) {
    const btnText = sendBtn.querySelector('.btn-text');
    const btnIcon = sendBtn.querySelector('.btn-icon');

    if (stopMode) {
        sendBtn.classList.add('stop-mode');
        btnText.textContent = 'Stop';
        btnIcon.textContent = '■';
    } else {
        sendBtn.classList.remove('stop-mode');
        btnText.textContent = 'Send';
        btnIcon.textContent = '→';
    }
}

async function stopChat() {
    if (!isProcessing || !currentSession) return;

    try {
        await fetch('/api/chat/stop', {
            method: 'POST',
            headers: { 'X-Session': currentSession }
        });
    } catch (err) {
        console.error('Failed to stop:', err);
    }
}

sendBtn.addEventListener('click', () => {
    if (isProcessing) {
        stopChat();
    } else {
        sendMessage();
    }
});

// Auto-resize textarea
let lastTextareaHeight = 52; // min-height

function autoResize() {
    const oldHeight = lastTextareaHeight;
    messageInput.style.height = 'auto';
    const newHeight = Math.min(messageInput.scrollHeight, 200);
    messageInput.style.height = newHeight + 'px';
    messageInput.style.overflowY = messageInput.scrollHeight > 200 ? 'auto' : 'hidden';

    // Scroll to bottom when textarea grows (works in Chrome)
    if (newHeight > oldHeight && !userScrolledUp) {
        scrollToBottom();
    }
    lastTextareaHeight = newHeight;
}

messageInput.addEventListener('input', autoResize);

// Track user scroll and update button indicators
function updateScrollButtons() {
    const scrollUpBtn = document.getElementById('scrollUpBtn');
    const scrollDownBtn = document.getElementById('scrollDownBtn');
    const canScrollUp = chatContainer.scrollTop > 10;
    const canScrollDown = chatContainer.scrollHeight - chatContainer.scrollTop - chatContainer.clientHeight > 10;

    scrollUpBtn.classList.toggle('active', canScrollUp);
    scrollDownBtn.classList.toggle('active', canScrollDown);
}

chatContainer.addEventListener('scroll', () => {
    updateScrollButtons();
    if (programmaticScroll) return;
    const atBottom = chatContainer.scrollHeight - chatContainer.scrollTop - chatContainer.clientHeight < 150;
    userScrolledUp = !atBottom;
});

// Update buttons on resize too
new ResizeObserver(updateScrollButtons).observe(chatContainer);

// Initial update
setTimeout(updateScrollButtons, 100);

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
