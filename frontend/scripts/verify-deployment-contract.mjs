import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { stdout } from "node:process";

import {
  cacheControl,
  deploymentHeaders,
} from "../deploy/deployment-contract.mjs";

const nginxPath = resolve("deploy/nginx.conf");
const nginx = await readFile(nginxPath, "utf8");

verifySecurityPolicy();
verifyContract(nginx);

const mutations = [
  ["frame-ancestors 'none'; ", ""],
  [
    `~^/assets/ "${cacheControl.immutableAsset}";`,
    `~^/assets/ "${cacheControl.html}";`,
  ],
  ["try_files $uri =404;", "try_files $uri /index.html;"],
  ["try_files $uri $uri/ /index.html;", "try_files $uri $uri/ =404;"],
  ["listen 127.0.0.1:8081 default_server;", "listen 80 default_server;"],
  [
    "~^(?:http|https)$ $http_x_forwarded_proto;",
    "default $http_x_forwarded_proto;",
  ],
  [
    'add_header X-Frame-Options "DENY" always;',
    '# add_header X-Frame-Options "DENY" always;',
  ],
];

for (const [target, replacement] of mutations) {
  if (!nginx.includes(target)) {
    throw new Error(`变异测试目标不存在：${target}`);
  }
  const mutated = nginx.replace(target, replacement);
  let rejected = false;
  try {
    verifyContract(mutated);
  } catch {
    rejected = true;
  }
  if (!rejected) {
    throw new Error(`部署合同未拒绝破坏性变异：${target}`);
  }
}

stdout.write(
  `deployment contract verification: ${Object.keys(deploymentHeaders).length} security headers, history fallback, strict asset 404, split cache policy, ${mutations.length} adversarial mutations rejected\n`,
);

function verifyContract(source) {
  const activeLines = new Set(
    source
      .split(/\r?\n/)
      .map((line) => line.trim())
      .filter((line) => line && !line.startsWith("#")),
  );
  for (const [name, value] of Object.entries(deploymentHeaders)) {
    requireDirective(
      activeLines,
      `add_header ${name} "${value}" always;`,
      `Nginx 缺少生产响应头 ${name}`,
    );
  }

  for (const [directive, message] of [
    [
      `~^/assets/ "${cacheControl.immutableAsset}";`,
      "Nginx 缺少哈希资产 immutable 缓存策略",
    ],
    [`default "${cacheControl.html}";`, "Nginx 缺少 HTML no-store 策略"],
    ["location ^~ /assets/ {", "Nginx 缺少独立资产命名空间"],
    ["try_files $uri =404;", "Nginx 必须让缺失资产严格返回 404"],
    [
      "location ~ ^/(?:api|\\.well-known|health)(?:/|$) {",
      "Nginx 缺少后端路径代理边界",
    ],
    [
      "try_files $uri $uri/ /index.html;",
      "Nginx 缺少显式 SPA history fallback",
    ],
    [
      "listen 127.0.0.1:8081 default_server;",
      "应用边缘必须只绑定 loopback，并置于受信 TLS 边缘之后",
    ],
    [
      "~^(?:http|https)$ $http_x_forwarded_proto;",
      "应用边缘只可信任受约束的外部协议值",
    ],
    [
      "proxy_set_header X-Forwarded-Proto $yang_forwarded_proto;",
      "应用边缘必须把受约束的外部协议传给后端",
    ],
  ]) {
    requireDirective(activeLines, directive, message);
  }

  if (/^\s*listen\s+(?:80|443)\b/m.test(source)) {
    throw new Error("应用边缘不得在此配置中直接暴露公网 80/443");
  }
}

function verifySecurityPolicy() {
  const directives = new Map(
    deploymentHeaders["Content-Security-Policy"]
      .split(";")
      .map((part) => part.trim().split(/\s+/))
      .filter((parts) => parts[0])
      .map(([name, ...values]) => [name, values]),
  );
  const exactDirectives = new Map([
    ["default-src", ["'self'"]],
    ["base-uri", ["'none'"]],
    ["object-src", ["'none'"]],
    ["form-action", ["'self'"]],
    ["frame-ancestors", ["'none'"]],
    ["script-src", ["'self'"]],
    ["connect-src", ["'self'"]],
  ]);
  for (const [name, expected] of exactDirectives) {
    if (JSON.stringify(directives.get(name)) !== JSON.stringify(expected)) {
      throw new Error(
        `部署 CSP ${name} 必须精确为 ${expected.join(" ")}，实际为 ${(directives.get(name) || []).join(" ")}`,
      );
    }
  }
}

function requireDirective(activeLines, directive, message) {
  if (!activeLines.has(directive)) {
    throw new Error(`${message}：${directive}`);
  }
}
