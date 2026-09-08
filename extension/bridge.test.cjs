const { test } = require("node:test");
const assert = require("node:assert/strict");
const { readFileSync } = require("node:fs");
const vm = require("node:vm");
const { File } = require("node:buffer");

function event() {
  const listeners = [];
  return { addListener: fn => listeners.push(fn), fire: (...args) => listeners.forEach(fn => fn(...args)) };
}
function port() {
  return { onMessage: event(), onDisconnect: event(), sent: [],
    postMessage(message) { this.sent.push(message); },
    disconnect() { this.disconnected = true; this.onDisconnect.fire(); } };
}
function content(fetchImpl, scripts = [{ textContent: '{"CSRF_TOKEN":"12345678-1234-1234-1234-123456789abc"}' }]) {
  const channel = port();
  const window = {}; window.top = window;
  vm.runInNewContext(readFileSync(`${__dirname}/content.js`, "utf8"), {
    chrome: { runtime: { connect: () => channel } }, window,
    location: { origin: "https://www.vinted.fi" }, document: { scripts },
    fetch: fetchImpl, TextEncoder, TextDecoder, Uint8Array, FormData, File, atob, AbortSignal,
  });
  return async command => {
    const before = channel.sent.length;
    channel.onMessage.fire({ id: "test", command });
    for (let i = 0; i < 20 && channel.sent.length === before; i++) await new Promise(setImmediate);
    assert.equal(channel.sent.length, before + 1);
    return channel.sent.at(-1);
  };
}

test("content script makes authenticated same-origin requests without exporting tokens", async () => {
  let request;
  const execute = content(async (path, options) => {
    request = { path, options };
    return new Response(JSON.stringify({ user: { id: 42 } }), { status: 200 });
  });
  const reply = await execute({ action: "request", method: "GET", path: "/api/v2/users/current" });
  assert.equal(reply.result.body.user.id, 42);
  assert.equal(request.options.credentials, "include");
  assert.equal(request.options.redirect, "error");
  assert.equal(request.options.headers["x-csrf-token"], undefined);
  assert.ok(!JSON.stringify(reply).includes("12345678"));
});

test("mutation handlers preserve web normalization and read CSRF locally", async () => {
  let request;
  const execute = content(async (path, options) => {
    request = { path, options };
    return new Response("{}", { status: 200 });
  });
  const reply = await execute({ action: "request", method: "PUT", path: "/api/v2/item_upload/items/42",
    body: { item: { price: "12.50", title: "Käytetty" }, upload_session_id: "session" } });
  assert.ok(reply.result);
  assert.equal(request.options.headers["x-csrf-token"], "12345678-1234-1234-1234-123456789abc");
  const body = JSON.parse(request.options.body);
  assert.equal(body.item.price, 12.5);
  assert.equal(body.item.title, "Käytetty");
  assert.equal(body.item.temp_uuid, "session");
  assert.equal(body.push_up, false);
});

test("photo handler builds multipart content without a file system API", async () => {
  let form;
  const execute = content(async (path, options) => {
    assert.equal(path, "/api/v2/photos");
    form = options.body;
    return new Response('{"id":123}', { status: 200 });
  });
  await execute({ action: "photo", upload_session_id: "12345678-1234-1234-1234-123456789abc",
    file_name: "photo.jpg", media_type: "image/jpeg", bytes: Buffer.from("image bytes").toString("base64") });
  assert.equal(form.get("photo[type]"), "item");
  assert.equal(await form.get("photo[file]").text(), "image bytes");
});

test("rejects arbitrary code, foreign paths, wrong methods and ambiguous CSRF without fetching", async () => {
  let calls = 0;
  const execute = content(async () => { calls++; return new Response("{}"); });
  for (const command of [
    { action: "evaluate", script: "document.cookie" },
    { action: "request", method: "GET", path: "https://example.com/" },
    { action: "request", method: "POST", path: "/api/v2/users/current" },
    { action: "request", method: "GET", path: "/api/v2/item_upload/items/../42" },
    { action: "request", method: "GET", path: "/api/v2/users/current?secret=1" },
  ]) assert.ok((await execute(command)).error);
  assert.equal(calls, 0);
  const noToken = content(async () => { calls++; }, []);
  assert.ok((await noToken({ action: "request", method: "POST", path: "/api/v2/item_upload/items", body: {} })).error);
  assert.equal(calls, 0);
});

test("bounded responses and failed mutations are not replayed", async () => {
  let calls = 0;
  const execute = content(async () => { calls++; return new Response("x".repeat(4 * 1024 * 1024 + 1)); });
  assert.ok((await execute({ action: "request", method: "GET", path: "/api/v2/users/current" })).error);
  assert.equal(calls, 1);
  const failed = content(async () => { calls++; throw new Error("secret upstream detail"); });
  const reply = await failed({ action: "request", method: "POST", path: "/api/v2/item_upload/items", body: {} });
  assert.ok(reply.error);
  assert.ok(!reply.error.includes("secret"));
  assert.equal(calls, 2);
});

function background() {
  const native = port();
  const onConnect = event();
  vm.runInNewContext(readFileSync(`${__dirname}/background.js`, "utf8"), {
    chrome: { runtime: { id: "extension", connectNative: () => native, onConnect } },
    TextEncoder, URL, setTimeout: () => 1, clearTimeout() {},
  });
  function tab(id) {
    const channel = port();
    channel.name = "flea-vinted";
    channel.sender = { id: "extension", frameId: 0, tab: { id }, url: "https://www.vinted.fi/" };
    onConnect.fire(channel);
    return channel;
  }
  function send(id, command) {
    native.onMessage.fire({ type: "chunk", id, index: 0, total: 1, data: JSON.stringify({ id, command }) });
  }
  function reply() { return JSON.parse(native.sent.map(chunk => chunk.data).join("")); }
  return { native, tab, send, reply };
}

test("background routes chunked commands only to a single matching tab", () => {
  const bridge = background();
  bridge.send("none", { action: "ready" });
  assert.equal(bridge.reply().error, "tab_unavailable");
  bridge.native.sent.length = 0;
  const tab = bridge.tab(1);
  bridge.send("one", { action: "ready" });
  assert.equal(tab.sent[0].id, "one");
  tab.onMessage.fire({ id: "wrong", result: true });
  assert.equal(bridge.native.sent.length, 0);
  tab.onMessage.fire({ id: "one", result: true });
  assert.equal(bridge.reply().result, true);
  bridge.native.sent.length = 0;
  bridge.tab(2);
  bridge.send("many", { action: "ready" });
  assert.equal(bridge.reply().error, "tab_unavailable");
});

test("background safely chunks Unicode responses and rejects out of order input", () => {
  const bridge = background();
  const tab = bridge.tab(1);
  bridge.send("large", { action: "ready" });
  const result = "😀".repeat(150000);
  tab.onMessage.fire({ id: "large", result });
  assert.ok(bridge.native.sent.length > 1);
  for (const chunk of bridge.native.sent) {
    assert.ok(chunk.data.isWellFormed());
    assert.ok(chunk.data.length <= 128 * 1024);
  }
  assert.equal(bridge.reply().result, result);
  bridge.native.onMessage.fire({ type: "chunk", id: "bad", index: 1, total: 2, data: "{}" });
  assert.equal(bridge.native.disconnected, true);
});

test("closing an in-flight tab returns an uncertain outcome without replay", () => {
  const bridge = background();
  const tab = bridge.tab(1);
  bridge.send("mutation", { action: "request" });
  tab.disconnect();
  assert.match(bridge.reply().error, /inspect remote state/);
  const replacement = bridge.tab(2);
  assert.equal(replacement.sent.length, 0);
});
