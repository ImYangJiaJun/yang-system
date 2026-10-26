// 审计复现：执行当前 TypeScript 源码，替换网络/浏览器边界，不发网络请求。
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import vm from "node:vm";

const root = new URL("../../../", import.meta.url);
const requireFrontend = createRequire(new URL("frontend/package.json", root));
const ts = requireFrontend("typescript");
function loadSource(path, dependencies, globals = {}) {
  const filename = fileURLToPath(new URL(path, root));
  const source = ts.transpileModule(readFileSync(filename, "utf8"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  }).outputText;
  const exports = {};
  vm.runInNewContext(source, {
    exports,
    require(name) {
      assert.ok(Object.hasOwn(dependencies, name), `未声明的依赖 ${name}`);
      return dependencies[name];
    },
    AbortSignal, Response, ...globals,
  }, { filename });
  return exports;
}

function deferred() {
  let resolve;
  const promise = new Promise((done) => { resolve = done; });
  return { promise, resolve };
}

for (const boundary of ["clear", "new-login"]) {
  const pending = deferred();
  const { SessionController } = loadSource("frontend/src/engine/session/session-controller.ts", {
    "./lifecycle": {},
    "./auth-session": {
      discardLegacyStoredCredentials() {}, clearStoredSession() {}, persistTokenPair() {},
      restoreSessionFromCookie: () => pending.promise,
    },
    "./session-coordination": {},
  });
  const controller = new SessionController();
  const restore = controller.restoreFromCookie();
  if (boundary === "clear") controller.clearSession();
  else controller.beginSession({ accessToken: "new-session-fixture" });
  pending.resolve({ accessToken: "old-session-fixture" });
  await restore;
  assert.equal(controller.getSnapshot().token, "old-session-fixture");
  assert.equal(controller.getSnapshot().restoreState, "authenticated");
  console.log(`REPRODUCED: late restore overwrites ${boundary}`);
}

const pending = deferred();
const events = [];
const auth = loadSource("frontend/src/engine/session/auth-session.ts", {
  "./lifecycle": { refreshSession: () => pending.promise },
  "./refresh-lock": { withRefreshLock: (task) => task() },
  "../http/errors": { ApiError: class ApiError extends Error {} },
}, {
  window: { dispatchEvent: (event) => events.push(event.type) },
  CustomEvent: class CustomEvent { constructor(type) { this.type = type; } },
});
const restore = auth.restoreSessionFromCookie();
auth.clearStoredSession();
pending.resolve({ accessToken: "old-session-fixture" });
await restore;
assert.equal(auth.activeAccessToken(), "old-session-fixture");
assert.ok(events.includes("yang:session-refreshed"));
console.log("REPRODUCED: cleared auth-session accepts late refresh and emits refreshed event");
console.log("3 deterministic checks passed (vulnerable behavior reproduced; no backend bypass claim)");
