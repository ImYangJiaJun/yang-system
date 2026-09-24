import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { stdout } from "node:process";

import {
  cacheControl,
  deploymentHeaders,
} from "../deploy/deployment-contract.mjs";

// 应用边缘的容器内端口。2026-09-22 起监听地址从 `127.0.0.1:8081` 改为 `8081`
//（所有网卡）——因为「只听 loopback」与「用 `docker -p` 发布端口」技术上互斥：
// DNAT 会把流量送到 netns 的 eth0，而 loopback 绑定收不到。
// 「不暴露公网」的责任因此移到**编排层**，并由本脚本的下方检查保证它仍是
// **机械可证**的，而不是退化成文档约定。
const EDGE_PORT = "8081";

const nginxPath = resolve("deploy/nginx.conf");
const deployScriptPath = resolve("../deploy/deploy-blue-green.sh");
const nginx = await readFile(nginxPath, "utf8");

verifySecurityPolicy();
verifyContract(nginx);
await verifyEdgePublishIsLoopbackOnly();

const mutations = [
  ["frame-ancestors 'none'; ", ""],
  [
    `~^/assets/ "${cacheControl.immutableAsset}";`,
    `~^/assets/ "${cacheControl.html}";`,
  ],
  ["try_files $uri =404;", "try_files $uri /index.html;"],
  ["try_files $uri $uri/ /index.html;", "try_files $uri $uri/ =404;"],
  [`listen ${EDGE_PORT} default_server;`, "listen 80 default_server;"],
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
  `deployment contract verification: ${Object.keys(deploymentHeaders).length} security headers, history fallback, strict asset 404, split cache policy, loopback-default edge publish, ${mutations.length} adversarial mutations rejected\n`,
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
      `listen ${EDGE_PORT} default_server;`,
      `应用边缘必须监听 ${EDGE_PORT}；对外暴露范围由编排层的 -p ${"${BIND_ADDR}"}:<host>:${EDGE_PORT} 约束（默认 127.0.0.1）`,
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

/**
 * 应用边缘改为监听所有网卡后，「不暴露公网」由编排层的 `-p <bind>:<host>:<edge>`
 * 承担。这条必须是机械可证的：部署脚本里凡是发布应用边缘端口的地方，绑定地址
 * 只能是 `127.0.0.1` 或 `${BIND_ADDR}`（后者另需证明其**源码默认值**是 loopback）。
 *
 * 允许：`-p 127.0.0.1:<host>:<edge>`、`-p ${BIND_ADDR}:<host>:<edge>`（默认 loopback）。
 * 拒绝：`-p <host>:<edge>`（裸端口 = 任意网卡）、`-p 0.0.0.0:<host>:<edge>`（硬编码）。
 *
 * ⚠️ **本检查的边界，别误读**：它只证明**源码里的默认值**是 loopback，因此保证的是
 *    「默认不暴露」。它**不检查运行时环境变量**——`BIND_ADDR=0.0.0.0 ./deploy-blue-green.sh`
 *    是被允许的（运维显式覆盖），`deploy.ps1 -EdgeBindAddr 0.0.0.0` 同理。
 *    所以它**不构成**对公网暴露面的保证；暴露面由调用方负责，`cmd_cutover` 收尾会按
 *    容器**实际绑定**如实报告。
 */
async function verifyEdgePublishIsLoopbackOnly() {
  let source;
  try {
    source = await readFile(deployScriptPath, "utf8");
  } catch {
    throw new Error(
      `找不到部署脚本 ${deployScriptPath}：应用边缘已监听所有网卡，其对外暴露必须由该脚本约束在 loopback，无法在缺少该文件时证明`,
    );
  }

  // 只看可执行行：脚本头部注释里就有 `-p 127.0.0.1:<host_port>:8081` 这样的示例，
  // 不剥掉注释会让检查扫到示例文本（现在是恰好被 endsWith 滤掉，但那是巧合）。
  const executable = source
    .split(/\r?\n/)
    .filter((line) => !line.trimStart().startsWith("#"))
    .join("\n");

  const specs = [...executable.matchAll(/-p\s+"?([^"\s]+)"?/g)].map((match) =>
    match[1].replace(/^["']|["']$/g, ""),
  );
  const edgeSpecs = specs.filter((spec) => spec.endsWith(`:${EDGE_PORT}`));
  if (edgeSpecs.length === 0) {
    throw new Error(
      `部署脚本里找不到对应用边缘端口 ${EDGE_PORT} 的 -p 发布；无法证明其对外暴露被约束在 loopback`,
    );
  }

  const ALLOWED_BINDINGS = new Set(["127.0.0.1", "${BIND_ADDR}"]);
  for (const spec of edgeSpecs) {
    const bindAddress = spec.split(":")[0];
    if (!ALLOWED_BINDINGS.has(bindAddress)) {
      throw new Error(
        `部署脚本必须以 127.0.0.1 绑定应用边缘端口，实际写了「-p ${spec}」——` +
          "公网暴露必须由宿主机上的受信 TLS 边缘承担，不能由应用容器直接对外",
      );
    }
  }

  // 用了 ${BIND_ADDR} 变量就必须证明它的默认值是 loopback：否则有人把默认值改成
  // 0.0.0.0 就能悄悄把应用边缘暴露到公网，而上面的检查仍会放行。
  if (edgeSpecs.some((spec) => spec.split(":")[0] === "${BIND_ADDR}")) {
    const expected = 'BIND_ADDR="${BIND_ADDR:-127.0.0.1}"';
    if (!source.includes(expected)) {
      throw new Error(
        `部署脚本使用了 \${BIND_ADDR} 发布应用边缘，但找不到 loopback 默认值声明：${expected}`,
      );
    }
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
