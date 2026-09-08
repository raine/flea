"use strict";
const maxBytes = 16 * 1024 * 1024;
const chunkSize = 128 * 1024;
const tabs = new Map();
let nativePort = null;
let incoming = null;
let pending = null;

function reply(message) {
  if (!nativePort) return;
  let text = JSON.stringify(message);
  if (new TextEncoder().encode(text).length > maxBytes) {
    text = JSON.stringify({ id: message.id, error: "Vinted response exceeded the bridge limit" });
  }
  const parts = [];
  for (let start = 0; start < text.length;) {
    let end = Math.min(start + chunkSize, text.length);
    const last = text.charCodeAt(end - 1);
    if (end < text.length && last >= 0xD800 && last <= 0xDBFF) end--;
    parts.push(text.slice(start, end));
    start = end;
  }
  for (let index = 0; index < parts.length; index++) {
    nativePort.postMessage({ type: "chunk", id: message.id, index, total: parts.length, data: parts[index] });
  }
}

function finish(message) {
  if (!pending || pending.id !== message.id) return;
  clearTimeout(pending.timer);
  pending = null;
  reply(message);
}

function dispatch(message) {
  if (typeof message.id !== "string" || !message.command) return;
  if (pending) {
    reply({ id: message.id, error: "A Vinted command is already running" });
    return;
  }
  if (tabs.size !== 1) {
    reply({ id: message.id, error: "tab_unavailable" });
    return;
  }
  const port = [...tabs.values()][0];
  pending = {
    id: message.id, port,
    timer: setTimeout(() => finish({ id: message.id, error: "Vinted request timed out; inspect remote state before retrying" }), 58000),
  };
  try { port.postMessage(message); }
  catch { finish({ id: message.id, error: "Vinted tab disconnected; inspect remote state before retrying" }); }
}

function receive(chunk) {
  if (!chunk || chunk.type !== "chunk" || typeof chunk.id !== "string" || chunk.id.length > 128 ||
      !Number.isInteger(chunk.index) || !Number.isInteger(chunk.total) || chunk.total < 1 ||
      chunk.total > 512 || typeof chunk.data !== "string" || chunk.data.length > chunkSize) {
    nativePort?.disconnect();
    return;
  }
  if (chunk.index === 0 && !incoming) {
    incoming = { id: chunk.id, total: chunk.total, parts: [], bytes: 0,
      timer: setTimeout(() => { incoming = null; nativePort?.disconnect(); }, 60000) };
  }
  if (!incoming || incoming.id !== chunk.id || incoming.total !== chunk.total || chunk.index !== incoming.parts.length) {
    nativePort?.disconnect();
    return;
  }
  incoming.bytes += new TextEncoder().encode(chunk.data).length;
  if (incoming.bytes > maxBytes) { nativePort?.disconnect(); return; }
  incoming.parts.push(chunk.data);
  if (incoming.parts.length === incoming.total) {
    const text = incoming.parts.join("");
    const id = incoming.id;
    clearTimeout(incoming.timer);
    incoming = null;
    try {
      const message = JSON.parse(text);
      if (message.id !== id) throw new Error("Mismatched ID");
      dispatch(message);
    } catch { reply({ id, error: "Invalid bridge request" }); }
  }
}

function connectNative() {
  if (nativePort) return;
  const port = chrome.runtime.connectNative("app.flea.bridge");
  nativePort = port;
  port.onMessage.addListener(receive);
  port.onDisconnect.addListener(() => {
    // Reading lastError prevents Chrome from logging an unhandled port error.
    void chrome.runtime.lastError;
    nativePort = null;
    if (incoming) clearTimeout(incoming.timer);
    incoming = null;
    if (pending) clearTimeout(pending.timer);
    pending = null;
    // Reconnection restores transport only. Commands are never replayed.
    setTimeout(connectNative, 2000);
  });
}

chrome.runtime.onConnect.addListener(port => {
  const sender = port.sender;
  if (port.name !== "flea-vinted" || sender?.id !== chrome.runtime.id ||
      sender.frameId !== 0 || !sender.tab ||
      new URL(sender.url).origin !== "https://www.vinted.fi") {
    port.disconnect();
    return;
  }
  const tabId = sender.tab.id;
  const previous = tabs.get(tabId);
  if (previous) previous.disconnect();
  tabs.set(tabId, port);
  port.onMessage.addListener(message => {
    if (pending?.port === port && message?.id === pending.id) finish(message);
  });
  port.onDisconnect.addListener(() => {
    if (tabs.get(tabId) === port) tabs.delete(tabId);
    if (pending?.port === port) finish({ id: pending.id, error: "Vinted tab navigated or closed; inspect remote state before retrying" });
  });
  connectNative();
});
connectNative();
