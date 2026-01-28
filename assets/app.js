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
            const messages = data.messages;
            for (let i = 0; i < messages.length; i++) {
                const msg = messages[i];
                if (msg.role === 'tool') {
                    // Parse tool message JSON
                    try {
                        const toolData = JSON.parse(msg.content);
                        const toolName = toolData.tool;
                        const toolInput = toolData.input || {};

                        // Check if this was an interactive tool (show as completed)
                        if (isInteractiveTool(toolName)) {
                            historyToolGroup = null;
                            document.getElementById('emptyState')?.remove();

                            if (toolName === 'AskUserQuestion' && toolInput.questions) {
                                // Render same as live, but without submit button
                                // User's answer is visible in the next user bubble
                                const wrapper = document.createElement('div');
                                wrapper.className = 'interactive-container';
                                chatContainer.appendChild(wrapper);
                                renderAskUserQuestion(wrapper, toolInput, null);
                                wrapper.querySelector('.question-submit-btn')?.remove();
                                wrapper.querySelector('.interactive-tool')?.classList.add('submitted');
                            } else if (toolName === 'ExitPlanMode') {
                                // Render same as live, but without buttons
                                const wrapper = document.createElement('div');
                                wrapper.className = 'interactive-container';
                                chatContainer.appendChild(wrapper);
                                renderExitPlanMode(wrapper, toolInput, null);
                                wrapper.querySelector('.plan-buttons')?.remove();
                                wrapper.querySelector('.interactive-tool')?.classList.add('submitted');
                            } else {
                                const completedChip = document.createElement('div');
                                completedChip.className = 'tool-chip interactive-completed';
                                completedChip.innerHTML = `<span class="tool-icon">${getToolIcon(toolName)}</span><span class="tool-name">${toolName}</span><span class="tool-detail">(completed)</span>`;
                                chatContainer.appendChild(completedChip);
                            }
                        } else {
                            // Group consecutive non-interactive tools
                            if (!historyToolGroup) {
                                historyToolGroup = document.createElement('div');
                                historyToolGroup.className = 'tool-group';
                                chatContainer.appendChild(historyToolGroup);
                                document.getElementById('emptyState')?.remove();
                            }
                            addToolChip(historyToolGroup, toolName, toolInput);
                        }
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
        'ExitPlanMode': '📋',
        // Notebooks
        'NotebookEdit': '📓',
        // Multi-file
        'MultiEdit': '📑',
    };
    return icons[toolName] || '🔧';
}

// Interactive tools that need special UI
const INTERACTIVE_TOOLS = ['AskUserQuestion', 'ExitPlanMode'];

function isInteractiveTool(toolName) {
    return INTERACTIVE_TOOLS.includes(toolName);
}

// Render AskUserQuestion UI
function renderAskUserQuestion(container, toolInput, toolUseId) {
    const questions = toolInput.questions || [];

    const questionContainer = document.createElement('div');
    questionContainer.className = 'interactive-tool ask-user-question';

    questions.forEach((q, qIndex) => {
        const questionDiv = document.createElement('div');
        questionDiv.className = 'question-block';

        // Question header/label
        if (q.header) {
            const header = document.createElement('div');
            header.className = 'question-header';
            header.textContent = q.header;
            questionDiv.appendChild(header);
        }

        // Question text
        const questionText = document.createElement('div');
        questionText.className = 'question-text';
        questionText.textContent = q.question;
        questionDiv.appendChild(questionText);

        // Options
        const optionsDiv = document.createElement('div');
        optionsDiv.className = 'question-options';

        const inputType = q.multiSelect ? 'checkbox' : 'radio';
        const inputName = `question-${qIndex}`;

        (q.options || []).forEach((opt, optIndex) => {
            const optionLabel = document.createElement('label');
            optionLabel.className = 'question-option';

            const input = document.createElement('input');
            input.type = inputType;
            input.name = inputName;
            input.value = opt.label;
            input.dataset.questionIndex = qIndex;
            input.dataset.optionIndex = optIndex;

            const labelText = document.createElement('span');
            labelText.className = 'option-label';
            labelText.textContent = opt.label;

            optionLabel.appendChild(input);
            optionLabel.appendChild(labelText);

            if (opt.description) {
                const desc = document.createElement('span');
                desc.className = 'option-description';
                desc.textContent = opt.description;
                optionLabel.appendChild(desc);
            }

            optionsDiv.appendChild(optionLabel);
        });

        // "Other" option with text input
        const otherLabel = document.createElement('label');
        otherLabel.className = 'question-option other-option';

        const otherInput = document.createElement('input');
        otherInput.type = inputType;
        otherInput.name = inputName;
        otherInput.value = '__other__';
        otherInput.dataset.questionIndex = qIndex;

        const otherText = document.createElement('span');
        otherText.className = 'option-label';
        otherText.textContent = 'Other:';

        const otherTextInput = document.createElement('input');
        otherTextInput.type = 'text';
        otherTextInput.className = 'other-text-input';
        otherTextInput.placeholder = 'Type your answer...';

        // Enable text input when "Other" is selected
        otherInput.addEventListener('change', () => {
            if (otherInput.checked) otherTextInput.focus();
        });

        // Auto-select "Other" when typing in text field
        otherTextInput.addEventListener('input', () => {
            if (otherTextInput.value.trim()) {
                otherInput.checked = true;
            }
        });

        // Submit on Enter
        otherTextInput.addEventListener('keydown', (e) => {
            if (e.key === 'Enter') {
                e.preventDefault();
                questionContainer.querySelector('.question-submit-btn')?.click();
            }
        });

        otherLabel.appendChild(otherInput);
        otherLabel.appendChild(otherText);
        otherLabel.appendChild(otherTextInput);
        optionsDiv.appendChild(otherLabel);

        questionDiv.appendChild(optionsDiv);
        questionContainer.appendChild(questionDiv);
    });

    // Submit button
    const submitBtn = document.createElement('button');
    submitBtn.className = 'question-submit-btn';
    submitBtn.textContent = 'Submit Answer';
    submitBtn.addEventListener('click', () => submitQuestionResponse(questionContainer, toolUseId, questions));
    questionContainer.appendChild(submitBtn);

    container.appendChild(questionContainer);
    scrollToBottom();
}

// Render ExitPlanMode UI
function renderExitPlanMode(container, toolInput, toolUseId) {
    const planContainer = document.createElement('div');
    planContainer.className = 'interactive-tool exit-plan-mode';

    const header = document.createElement('div');
    header.className = 'plan-header';
    header.textContent = '📋 Plan Ready for Review';
    planContainer.appendChild(header);

    const description = document.createElement('div');
    description.className = 'plan-description';
    description.textContent = 'Claude has finished planning. Review the plan and approve to proceed with implementation.';
    planContainer.appendChild(description);

    const buttonsDiv = document.createElement('div');
    buttonsDiv.className = 'plan-buttons';

    const approveBtn = document.createElement('button');
    approveBtn.className = 'plan-btn approve';
    approveBtn.textContent = '✓ Approve Plan';
    approveBtn.addEventListener('click', () => submitPlanResponse(planContainer, toolUseId, true));

    const rejectBtn = document.createElement('button');
    rejectBtn.className = 'plan-btn reject';
    rejectBtn.textContent = '✗ Request Changes';
    rejectBtn.addEventListener('click', () => submitPlanResponse(planContainer, toolUseId, false));

    buttonsDiv.appendChild(approveBtn);
    buttonsDiv.appendChild(rejectBtn);
    planContainer.appendChild(buttonsDiv);

    container.appendChild(planContainer);
    scrollToBottom();
}

// Submit response to AskUserQuestion - sends as normal chat message
function submitQuestionResponse(container, toolUseId, questions) {
    const selectedValues = [];

    questions.forEach((q, qIndex) => {
        const inputs = container.querySelectorAll(`input[data-question-index="${qIndex}"]:checked`);

        const values = Array.from(inputs).map(input => {
            if (input.value === '__other__') {
                const otherText = input.parentElement.querySelector('.other-text-input');
                const text = otherText ? otherText.value.trim() : '';
                return text ? 'Other: ' + text : '';
            }
            return input.value;
        }).filter(v => v);

        selectedValues.push(...values);
    });

    if (selectedValues.length === 0) return;

    // Hide submit button, keep visual appearance
    container.querySelector('.question-submit-btn')?.remove();
    container.classList.add('submitted');

    const answerText = selectedValues.join(', ');

    // Send as normal chat message
    messageInput.value = answerText;
    sendMessage();
}

// Submit response to ExitPlanMode - sends as normal chat message
function submitPlanResponse(container, toolUseId, approved) {
    // Hide buttons, keep visual appearance
    container.querySelector('.plan-buttons')?.remove();
    container.classList.add('submitted');

    // Show status
    const status = document.createElement('div');
    status.className = 'plan-status';
    status.textContent = approved ? '✓ Plan Approved' : '✗ Changes Requested';
    container.appendChild(status);

    // If approved, switch to execute mode
    if (approved) {
        planMode = false;
        updatePlanModeUI();
    }

    // Send as normal chat message
    messageInput.value = approved ? 'Yes, proceed with the plan.' : 'No, I\'d like changes to the plan.';
    sendMessage();
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

// Poll for response blocks from Claude
async function pollResponses(sessionId) {
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
            if (block.content !== lastText) {
                currentToolGroup = null;
                currentMsg = createMessageElement('assistant');
                renderMarkdown(currentMsg, block.content);
                lastText = block.content;
            }
        } else if (block.type === 'tool') {
            const toolName = block.tool || 'tool';
            const toolInput = block.input || {};
            const toolUseId = block.tool_use_id;

            console.log('Tool block received:', { toolName, toolUseId, block });

            if (isInteractiveTool(toolName) && toolUseId) {
                currentToolGroup = null;

                const interactiveContainer = document.createElement('div');
                interactiveContainer.className = 'interactive-container';
                chatContainer.appendChild(interactiveContainer);
                document.getElementById('emptyState')?.remove();

                if (toolName === 'AskUserQuestion') {
                    renderAskUserQuestion(interactiveContainer, toolInput, toolUseId);
                } else if (toolName === 'ExitPlanMode') {
                    renderExitPlanMode(interactiveContainer, toolInput, toolUseId);
                }

                // Stop polling and backend process - user needs to answer via new sendMessage
                fetch('/api/chat/stop', { method: 'POST', headers: { 'X-Session': sessionId } });
                more = false;
                break;
            } else {
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
            }
        } else if (block.type === 'error') {
            currentToolGroup = null;
            const errMsg = createMessageElement('system');
            errMsg.textContent = block.content;
        }

        more = block.more;
    }

    typingIndicator.classList.remove('active');
    isProcessing = false;
    setButtonMode(false);

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

        // Poll for responses
        await pollResponses(sessionId);

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
