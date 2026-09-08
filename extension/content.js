(() => {
  "use strict";
  const origin = "https://www.vinted.fi";
  const maxBytes = 4 * 1024 * 1024;
  const routes = [
    ["GET", /^\/api\/v2\/users\/current$/],
    ["GET", /^\/api\/v2\/item_upload\/items\/[0-9]+$/],
    ["PUT", /^\/api\/v2\/item_upload\/items\/[0-9]+$/],
    ["POST", /^\/api\/v2\/item_upload\/(items|drafts)$/],
    ["PUT", /^\/api\/v2\/item_upload\/drafts\/[0-9]+$/],
    ["DELETE", /^\/api\/v2\/item_upload\/drafts\/[0-9]+$/],
    ["POST", /^\/api\/v2\/item_upload\/drafts\/[0-9]+\/completion$/],
  ];

  function csrfToken() {
    const tokens = new Set();
    for (const script of document.scripts) {
      if (script.src) continue;
      for (const match of script.textContent.matchAll(/"CSRF_TOKEN\\?":\\?"([a-f0-9-]{36})\\?"/gi)) {
        tokens.add(match[1]);
      }
    }
    if (tokens.size !== 1) throw new Error("CSRF token unavailable");
    return [...tokens][0];
  }

  async function responseBody(response) {
    const reader = response.body?.getReader();
    const chunks = [];
    let size = 0;
    if (reader) {
      for (;;) {
        const { value, done } = await reader.read();
        if (done) break;
        size += value.byteLength;
        if (size > maxBytes) {
          await reader.cancel();
          throw new Error("Response too large");
        }
        chunks.push(value);
      }
    }
    const bytes = new Uint8Array(size);
    let offset = 0;
    for (const chunk of chunks) {
      bytes.set(chunk, offset);
      offset += chunk.length;
    }
    const text = new TextDecoder().decode(bytes);
    let body = {};
    if (text) {
      try { body = JSON.parse(text); }
      catch { body = { message: "Vinted returned a non-JSON browser response" }; }
    }
    return { status: response.status, body };
  }

  async function execute(command) {
    if (location.origin !== origin || window.top !== window) throw new Error("Unexpected page");
    if (command.action === "ready") return true;
    const headers = { accept: "application/json,text/plain,*/*,image/webp" };
    let path;
    let method;
    let body;
    if (command.action === "photo") {
      if (typeof command.bytes !== "string" || command.bytes.length > 12 * 1024 * 1024 ||
          !/^[0-9a-f-]{36}$/i.test(command.upload_session_id) ||
          !["image/jpeg", "image/png"].includes(command.media_type) ||
          typeof command.file_name !== "string" || command.file_name.length > 255) {
        throw new Error("Invalid photo");
      }
      path = "/api/v2/photos";
      method = "POST";
      headers["x-csrf-token"] = csrfToken();
      const bytes = Uint8Array.from(atob(command.bytes), character => character.charCodeAt(0));
      body = new FormData();
      body.append("photo[type]", "item");
      body.append("upload_session_id", command.upload_session_id);
      body.append("photo[file]", new File([bytes], command.file_name, { type: command.media_type }));
    } else if (command.action === "request") {
      ({ path, method } = command);
      if (typeof path !== "string" || !routes.some(([verb, pattern]) => verb === method && pattern.test(path))) {
        throw new Error("Unsupported request");
      }
      if (method !== "GET") headers["x-csrf-token"] = csrfToken();
      if (command.body != null) {
        if (method === "GET" || typeof command.body !== "object" || Array.isArray(command.body)) {
          throw new Error("Invalid request body");
        }
        headers["content-type"] = "application/json";
        headers["x-upload-form"] = "true";
        headers["x-enable-dynamic-attribute-condition"] = "true";
        headers["x-enable-dynamic-attribute-size"] = "true";
        headers["x-enable-dynamic-attribute-video-game-rating"] = "true";
        const value = command.body;
        const item = value.item || value.draft;
        if (item && typeof item.price === "string") item.price = Number(item.price);
        if (item && value.upload_session_id) item.temp_uuid = value.upload_session_id;
        if (value.push_up == null) value.push_up = false;
        body = JSON.stringify(value);
        if (new TextEncoder().encode(body).length > maxBytes) throw new Error("Request too large");
      }
    } else {
      throw new Error("Unsupported command");
    }
    return responseBody(await fetch(path, {
      method, headers, body, credentials: "include", redirect: "error",
      signal: AbortSignal.timeout(55000),
    }));
  }

  const port = chrome.runtime.connect({ name: "flea-vinted" });
  port.onMessage.addListener(message => {
    if (!message || typeof message.id !== "string") return;
    execute(message.command).then(
      result => port.postMessage({ id: message.id, result }),
      () => port.postMessage({ id: message.id, error: "Vinted request failed; inspect the tab and remote state before retrying" }),
    );
  });
})();
