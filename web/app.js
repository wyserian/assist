// assist web UI: chat plus dashboard over the daemon's WebSocket.
// All text from the daemon is set with textContent, never parsed as HTML.
"use strict";

const pending = new Map(); // rpc id -> {resolve, reject}
const chatJobs = new Set(); // jobs started from this page's chat
let socket = null;

const $ = (id) => document.getElementById(id);

function el(tag, text, cls) {
  const node = document.createElement(tag);
  if (text !== undefined) node.textContent = String(text);
  if (cls) node.className = cls;
  return node;
}

function row(cells) {
  const tr = document.createElement("tr");
  for (const cell of cells) {
    const td = document.createElement("td");
    if (cell instanceof Node) td.append(cell);
    else td.textContent = cell === undefined || cell === null ? "" : String(cell);
    tr.append(td);
  }
  return tr;
}

function button(label, onClick, cls) {
  const b = el("button", label, cls);
  b.type = "button";
  b.addEventListener("click", onClick);
  return b;
}

function time(ms) {
  return ms ? new Date(ms).toLocaleString() : "";
}

function setStatus(text) {
  $("status").textContent = text;
}

// --- protocol ---

function envelope(kind, body) {
  return { v: 1, id: crypto.randomUUID(), kind, from: "web", to: "hub", ts: Date.now(), body };
}

function send(kind, body) {
  const message = envelope(kind, body);
  socket.send(JSON.stringify(message));
  return message.id;
}

function rpc(method, params) {
  return new Promise((resolve, reject) => {
    if (!socket || socket.readyState !== WebSocket.OPEN) {
      reject(new Error("not connected"));
      return;
    }
    const id = send("rpc", { method, params: params === undefined ? null : params });
    pending.set(id, { resolve, reject });
  });
}

function onMessage(event) {
  let message;
  try {
    message = JSON.parse(event.data);
  } catch {
    return;
  }
  if (message.kind === "rpc_result" && pending.has(message.re)) {
    const { resolve, reject } = pending.get(message.re);
    pending.delete(message.re);
    const body = message.body || {};
    if (body.error) reject(new Error(body.error.message));
    else resolve(body.result);
  } else if (message.kind === "reply") {
    showReply(message.body || {});
  } else if (message.kind === "job_update") {
    showUpdate(message.body || {});
  }
}

async function signIn() {
  const match = location.hash.match(/^#otp=([0-9a-f]+)$/);
  if (!match) return;
  history.replaceState(null, "", location.pathname);
  await fetch("/auth", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ code: match[1] }),
  });
}

function connect() {
  let opened = false;
  socket = new WebSocket(`ws://${location.host}/ws`);
  socket.addEventListener("open", () => {
    opened = true;
    $("signin").hidden = true;
    setStatus("Connected");
    send("hello", { client: "assist-web", version: "1" });
    send("subscribe", { topics: ["jobs"] });
    refresh(currentTab());
  });
  socket.addEventListener("message", onMessage);
  socket.addEventListener("close", () => {
    for (const { reject } of pending.values()) reject(new Error("disconnected"));
    pending.clear();
    if (!opened) {
      setStatus("Not signed in");
      $("signin").hidden = false;
      return;
    }
    setStatus("Disconnected, retrying");
    setTimeout(connect, 2000);
  });
}

// --- chat ---

function logLine(text, cls) {
  const log = $("log");
  log.append(el("div", text, cls));
  log.scrollTop = log.scrollHeight;
}

function showReply(reply) {
  for (const line of reply.say || []) logLine(line);
  for (const job of reply.jobs || []) {
    chatJobs.add(job.id);
    logLine(`[#${job.id}] ${job.title}`);
  }
}

function showUpdate(update) {
  if (update.type === "output" && chatJobs.has(update.id)) {
    const mark = update.stream === "err" ? "! " : "";
    logLine(`[#${update.id}] ${mark}${update.text}`, update.stream === "err" ? "err" : undefined);
  }
  if (update.type === "state" && !["queued", "running", "paused"].includes(update.state)) {
    if (chatJobs.has(update.id)) {
      const reason = update.error ? `: ${update.error}` : "";
      logLine(`[#${update.id}] ${update.state}${reason}`, update.state === "done" ? undefined : "err");
      chatJobs.delete(update.id);
    }
    if (currentTab() === "jobs") refreshSoon();
  }
}

$("say").addEventListener("submit", (event) => {
  event.preventDefault();
  const input = $("say-text");
  const text = input.value.trim();
  if (!text || !socket || socket.readyState !== WebSocket.OPEN) return;
  logLine(`> ${text}`, "you");
  send("utterance", { text });
  input.value = "";
});

// --- tabs ---

function currentTab() {
  const active = document.querySelector("#tabs button[aria-pressed='true']");
  return active ? active.dataset.tab : "chat";
}

for (const tab of document.querySelectorAll("#tabs button")) {
  tab.addEventListener("click", () => {
    for (const other of document.querySelectorAll("#tabs button")) {
      const on = other === tab;
      other.setAttribute("aria-pressed", String(on));
      $(other.dataset.tab).hidden = !on;
    }
    refresh(tab.dataset.tab);
  });
}

let refreshTimer = null;
function refreshSoon() {
  clearTimeout(refreshTimer);
  refreshTimer = setTimeout(() => refresh(currentTab()), 300);
}

function refresh(tab) {
  const views = {
    jobs: loadJobs,
    waiting: loadWaiting,
    secrets: loadSecrets,
    commands: loadMisses,
    settings: loadSettings,
    audit: loadAudit,
    clients: loadClients,
  };
  if (views[tab]) views[tab]().catch((err) => setStatus(err.message));
}

// --- waiting ---

async function loadWaiting() {
  const questions = await rpc("dialogs.pending");
  const body = $("waiting-body");
  body.replaceChildren();
  for (const item of questions) {
    const answer = async (text) => {
      try {
        const reply = await rpc("dialogs.answer", { client: item.client, text });
        const started = (reply.jobs || []).map((job) => `#${job.id}`).join(", ");
        setStatus(started ? `Answered; started ${started}` : "Answered");
      } catch (err) {
        setStatus(err.message);
      }
      loadWaiting();
    };
    const form = document.createElement("form");
    const input = el("input");
    input.maxLength = 8000;
    input.setAttribute("aria-label", "Answer");
    form.append(input, el("button", "Send"));
    form.addEventListener("submit", (event) => {
      event.preventDefault();
      if (input.value.trim()) answer(input.value.trim());
    });
    const buttons = document.createElement("span");
    buttons.append(button("Yes", () => answer("yes")), " ", button("No", () => answer("no")), form);
    body.append(row([`${item.kind} ${item.program} (${item.client})`, item.question, time(item.asked), buttons]));
  }
  if (!questions.length) body.append(row(["Nothing is waiting."]));
}

// Questions come and go on other clients; poll while the tab is open.
setInterval(() => {
  if (currentTab() === "waiting" && socket && socket.readyState === WebSocket.OPEN) refresh("waiting");
}, 3000);

// --- jobs ---

async function loadJobs() {
  const jobs = await rpc("jobs.list");
  const body = $("jobs-body");
  body.replaceChildren();
  for (const job of jobs.slice().reverse()) {
    const active = ["queued", "running", "paused"].includes(job.state);
    const actions = document.createElement("span");
    actions.append(button("Log", () => showLog(job)));
    const act = (method) => rpc(method, { id: job.id }).then(loadJobs).catch((err) => setStatus(err.message));
    if (job.state === "running") actions.append(" ", button("Pause", () => act("jobs.pause")));
    if (job.state === "paused") actions.append(" ", button("Resume", () => act("jobs.resume")));
    if (active) actions.append(" ", button("Stop", () => act("jobs.stop"), "danger"));
    const usage = job.cpu === undefined ? "" : ` (${job.cpu}% CPU, ${Math.round(job.memory / 1048576)} MB)`;
    body.append(row([`#${job.id}`, job.state + usage, job.lane, job.source, job.title, actions]));
  }
  if (!jobs.length) body.append(row(["No jobs yet."]));
}

async function showLog(job) {
  const lines = await rpc("jobs.tail", { id: job.id });
  $("job-log-title").textContent = `#${job.id} ${job.title}`;
  $("job-log").textContent = lines.length ? lines.join("\n") : "(no output)";
  $("job-log-title").hidden = false;
  $("job-log").hidden = false;
}

$("jobs-refresh").addEventListener("click", () => refresh("jobs"));

$("stop-all").addEventListener("click", async () => {
  if (!confirm("Stop every queued and running job?")) return;
  try {
    const stopped = await rpc("jobs.stop_all");
    setStatus(`Stopped ${stopped} job(s)`);
    refresh(currentTab());
  } catch (err) {
    setStatus(err.message);
  }
});

// --- secrets ---

async function loadSecrets() {
  const body = $("secrets-body");
  body.replaceChildren();
  let secrets;
  try {
    secrets = await rpc("secrets.list");
  } catch (err) {
    $("secrets-note").textContent = err.message;
    return;
  }
  $("secrets-note").textContent = "Values are stored in the system keystore and never shown.";
  for (const secret of secrets) {
    const remove = button("Delete", async () => {
      if (!confirm(`Delete the secret ${secret.name}?`)) return;
      await rpc("secrets.delete", { name: secret.name });
      loadSecrets();
    }, "danger");
    body.append(row([secret.name, `...${secret.last4}`, time(secret.set), remove]));
  }
  if (!secrets.length) body.append(row(["No secrets."]));
}

$("secret-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  const name = $("secret-name").value.trim();
  const value = $("secret-value");
  try {
    await rpc("secrets.set", { name, value: value.value });
    $("secrets-note").textContent = `Saved ${name}.`;
    value.value = "";
    loadSecrets();
  } catch (err) {
    $("secrets-note").textContent = err.message;
  }
});

// --- commands ---

$("explain-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  const out = $("explain-out");
  try {
    const result = await rpc("commands.explain", { text: $("explain-text").value });
    if (!result) {
      out.textContent = "No command matches that.";
      return;
    }
    const lines = [`command:  ${result.command}`];
    for (const [slot, value] of Object.entries(result.slots)) lines.push(`  ${slot} = ${value}`);
    for (const [slot, options] of Object.entries(result.ambiguous || {})) {
      lines.push(`  ${slot} could be: ${options.join(", ")}`);
    }
    lines.push(`action:   ${result.readback}`, `decision: ${result.decision}`);
    out.textContent = lines.join("\n");
  } catch (err) {
    out.textContent = err.message;
  }
});

async function loadMisses() {
  const [misses, commands] = await Promise.all([rpc("misses.list"), rpc("commands.list")]);
  const body = $("misses-body");
  body.replaceChildren();
  for (const miss of misses) {
    const select = document.createElement("select");
    select.setAttribute("aria-label", "Command");
    for (const command of commands) {
      const option = el("option", command.name);
      option.value = command.name;
      option.selected = command.name === miss.guess;
      select.append(option);
    }
    const phrase = el("input");
    phrase.value = miss.text;
    phrase.maxLength = 200;
    phrase.setAttribute("aria-label", "Phrase");
    const slots = el("div", "", "hint");
    const showSlots = () => {
      const command = commands.find((c) => c.name === select.value);
      const names = command ? Object.keys(command.slots).map((s) => `.${s}.`) : [];
      slots.textContent = names.length ? `slots: ${names.join(" ")}` : "no slots";
    };
    select.addEventListener("change", showSlots);
    showSlots();
    const add = button("Add phrase", async () => {
      try {
        await rpc("phrases.add", { command: select.value, phrase: phrase.value, miss: miss.text });
        $("misses-note").textContent = `Added "${phrase.value}" to ${select.value}.`;
        loadMisses();
      } catch (err) {
        $("misses-note").textContent = err.message;
      }
    });
    const dismiss = button("Dismiss", async () => {
      await rpc("misses.dismiss", { text: miss.text });
      loadMisses();
    });
    const picker = document.createElement("span");
    picker.append(select, slots);
    const actions = document.createElement("span");
    actions.append(add, " ", dismiss);
    body.append(row([miss.text, miss.count, picker, phrase, actions]));
  }
  if (!misses.length) body.append(row(["Everything said recently was understood."]));
}

// --- settings ---

async function saveSetting(key, value) {
  const note = $("settings-note");
  try {
    const result = await rpc("settings.set", { key, value });
    note.textContent = result.restart_needed
      ? `Saved ${key}. Restart the daemon to apply it.`
      : `Saved ${key}. It applies now.`;
  } catch (err) {
    note.textContent = err.message;
  }
  loadSettings();
}

async function loadSettings() {
  const settings = await rpc("settings.get");
  if (settings.restart_pending && !$("settings-note").textContent) {
    $("settings-note").textContent = "Some saved settings apply after the daemon restarts.";
  }
  const commands = $("settings-commands");
  commands.replaceChildren();
  for (const command of settings.commands) {
    const box = el("input");
    box.type = "checkbox";
    box.checked = command.risky;
    box.setAttribute("aria-label", `Ask before ${command.name}`);
    box.addEventListener("change", () => saveSetting(`commands.${command.name}.risky`, box.checked));
    commands.append(row([command.name, box]));
  }
  const fields = [
    ["lanes.interactive", "Quick jobs at once", settings.lanes_interactive, "number"],
    ["lanes.background", "Long jobs at once", settings.lanes_background, "number"],
    ["jobs.timeout", "Stop a job after (30s, 10m, 2h)", settings.jobs_timeout, "text"],
    ["jobs.output_tail_kb", "Output kept per job (KB)", settings.output_tail_kb, "number"],
    ["jobs.retention_days", "Keep job history (days)", settings.retention_days, "number"],
    ["jobs.log_cap_mb", "All job logs at most (MB)", settings.log_cap_mb, "number"],
    ["search.url", "Search URL ({query} is replaced)", settings.search_url, "text"],
  ];
  const values = $("settings-values");
  values.replaceChildren();
  for (const [key, label, current, type] of fields) {
    const input = el("input");
    input.type = type;
    input.value = current;
    input.setAttribute("aria-label", label);
    if (type === "number") input.min = "0";
    const save = button("Save", () => {
      const value = type === "number" ? Number(input.value) : input.value.trim();
      saveSetting(key, value);
    });
    const cell = document.createElement("span");
    cell.append(input, " ", save);
    values.append(row([label, cell]));
  }
}

// --- audit ---

async function loadAudit() {
  const entries = await rpc("audit.tail", { limit: 200 });
  const body = $("audit-body");
  body.replaceChildren();
  for (const entry of entries.slice().reverse()) {
    const event = entry.error ? `${entry.event}: ${entry.error}` : entry.event;
    body.append(row([time(entry.ts), event, `#${entry.job}`, entry.source, entry.title]));
  }
  if (!entries.length) body.append(row(["Nothing audited yet."]));
}

$("audit-refresh").addEventListener("click", () => refresh("audit"));

// --- clients ---

async function loadClients() {
  const clients = await rpc("clients.list");
  const body = $("clients-body");
  body.replaceChildren();
  for (const client of clients) {
    const revoke = button("Revoke", async () => {
      if (!confirm(`Disconnect client ${client.id}?`)) return;
      await rpc("clients.revoke", { id: client.id });
      loadClients();
    }, "danger");
    body.append(row([client.id, client.kind, client.client, time(client.connected), revoke]));
  }
}

$("clients-refresh").addEventListener("click", () => refresh("clients"));

signIn().finally(connect);
