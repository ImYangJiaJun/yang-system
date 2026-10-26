import assert from "node:assert/strict";
import { Buffer } from "node:buffer";
import { setTimeout } from "node:timers";
import { stdout } from "node:process";
import { execFileSync, spawnSync } from "node:child_process";
import { mkdtemp, readFile, writeFile, mkdir, rm } from "node:fs/promises";
import { createServer, request as httpRequest } from "node:http";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { gzipSync, gunzipSync } from "node:zlib";

// 使用生产镜像与配置，仅把上游地址改为本机的隔离测试服务。
const root = resolve(import.meta.dirname, "..");
const dockerfile = await readFile(join(root, "deploy/Dockerfile"), "utf8");
const image = dockerfile.match(/FROM (nginx:\S+) AS runtime/)[1];
const directory = await mkdtemp(join(tmpdir(), "yang-nginx-check-"));
let container;
const docker = (...args) =>
  execFileSync("docker", args, { encoding: "utf8", windowsHide: true }).trim();
const source = "export const example = 'compression fixture';\n".repeat(200);
const secret = "sensitive-probe-must-never-be-logged";
const receivedUploads = [];
const upstream = createServer(async (req, res) => {
  if (req.url.startsWith("/api/precompressed.js")) {
    res.writeHead(200, {
      "Content-Type": "application/javascript",
      "Content-Encoding": "gzip",
    });
    res.end(gzipSync(source));
  } else if (req.url.startsWith("/api/upload")) {
    let bytes = 0;
    for await (const chunk of req) bytes += chunk.length;
    receivedUploads.push(bytes);
    res.end(String(bytes));
  } else if (req.url.startsWith("/api/broken")) {
    req.socket.destroy();
  } else {
    res.writeHead(404);
    res.end();
  }
});

function request(port, path, encoding = "gzip", body, chunked = false) {
  return new Promise((resolveRequest, reject) => {
    const req = httpRequest(
      {
        hostname: "127.0.0.1",
        port,
        path,
        method: body ? "POST" : "GET",
        headers: {
          "Accept-Encoding": encoding,
          Referer: `https://example.test/?token=${secret}`,
          ...(body && {
            "Content-Type": "multipart/form-data; boundary=yang-upload-check",
            ...(chunked
              ? { "Transfer-Encoding": "chunked" }
              : { "Content-Length": body.length }),
          }),
        },
      },
      (response) => {
        const chunks = [];
        response.on("data", (chunk) => chunks.push(chunk));
        response.on("end", () =>
          resolveRequest({
            status: response.statusCode,
            headers: response.headers,
            body: Buffer.concat(chunks),
          }),
        );
        response.on("error", reject);
      },
    );
    req.setTimeout(10000, () =>
      req.destroy(new Error("Nginx request timeout")),
    );
    req.on("error", reject);
    req.end(body);
  });
}

try {
  await new Promise((resolveListen, reject) => {
    upstream.once("error", reject);
    // Docker Desktop 通过 host.docker.internal 访问人工测试数据；finally 关闭监听。
    upstream.listen(0, "0.0.0.0", resolveListen);
  });
  await mkdir(join(directory, "assets"));
  await writeFile(join(directory, "assets/main.js"), source);
  await writeFile(join(directory, "assets/image.png"), Buffer.alloc(2048, 7));
  await writeFile(
    join(directory, "index.html"),
    "<!doctype html><title>test</title>",
  );
  const config = (
    await readFile(join(root, "deploy/nginx.conf"), "utf8")
  ).replace(
    "server 127.0.0.1:8080;",
    `server host.docker.internal:${upstream.address().port};`,
  );
  await writeFile(join(directory, "nginx.conf"), config);
  container = docker(
    "run",
    "--rm",
    "-d",
    "-p",
    "127.0.0.1::8081",
    "-v",
    `${directory}:/usr/share/nginx/html:ro`,
    "-v",
    `${join(directory, "nginx.conf")}:/etc/nginx/nginx.conf:ro`,
    image,
  );
  assert.match(container, /^[a-f0-9]{64}$/);
  const port = Number(docker("port", container, "8081/tcp").split(":").at(-1));
  let compressed;
  for (let attempt = 0; attempt < 50; attempt += 1) {
    try {
      compressed = await request(port, "/assets/main.js");
      break;
    } catch (error) {
      if (attempt === 49) throw error;
      await new Promise((resolveWait) => setTimeout(resolveWait, 100));
    }
  }
  assert.equal(compressed.status, 200);
  assert.equal(compressed.headers["content-encoding"], "gzip");
  assert.match(compressed.headers.vary, /Accept-Encoding/i);
  assert.equal(gunzipSync(compressed.body).toString(), source);
  const identity = await request(port, "/assets/main.js", "identity");
  assert.equal(identity.headers["content-encoding"], undefined);
  assert.match(identity.headers.vary, /Accept-Encoding/i);
  assert.equal(identity.body.toString(), source);
  const png = await request(port, "/assets/image.png");
  assert.equal(png.headers["content-encoding"], undefined);
  const alreadyCompressed = await request(port, "/api/precompressed.js");
  assert.equal(alreadyCompressed.headers["content-encoding"], "gzip");
  assert.equal(gunzipSync(alreadyCompressed.body).toString(), source);
  const file = Buffer.alloc(16 * 1024 * 1024, 7);
  const fileHeader = Buffer.from(
    '--yang-upload-check\r\nContent-Disposition: form-data; name="file"; filename="fixture.xlsx"\r\nContent-Type: application/octet-stream\r\n\r\n',
  );
  const paddingHeader = Buffer.from(
    '\r\n--yang-upload-check\r\nContent-Disposition: form-data; name="padding"\r\n\r\n',
  );
  const end = Buffer.from("\r\n--yang-upload-check--\r\n");
  const ordinaryUpload = Buffer.concat([fileHeader, file, end]);
  const ordinaryResult = await request(
    port,
    `/api/upload?token=${secret}`,
    "identity",
    ordinaryUpload,
  );
  assert.equal(
    ordinaryResult.status,
    200,
    "16 MiB 文件及 multipart 开销应通过",
  );
  assert.equal(Number(ordinaryResult.body), ordinaryUpload.length);
  const totalLimit = 17 * 1024 * 1024;
  const exactUpload = Buffer.concat([
    fileHeader,
    file,
    paddingHeader,
    Buffer.alloc(totalLimit - ordinaryUpload.length - paddingHeader.length, 32),
    end,
  ]);
  assert.equal(exactUpload.length, totalLimit);
  for (const chunked of [false, true]) {
    const accepted = await request(
      port,
      `/api/upload?token=${secret}`,
      "identity",
      exactUpload,
      chunked,
    );
    assert.equal(accepted.status, 200, "17 MiB 总请求应通过");
    assert.equal(Number(accepted.body), totalLimit);
    const rejected = await request(
      port,
      `/api/upload?token=${secret}`,
      "identity",
      Buffer.concat([exactUpload, Buffer.from("x")]),
      chunked,
    );
    assert.equal(rejected.status, 413, "17 MiB + 1 字节总请求应拒绝");
  }
  assert.deepEqual(receivedUploads, [
    ordinaryUpload.length,
    totalLimit,
    totalLimit,
  ]);
  await request(port, `/assets/${secret}.js?token=${secret}`);
  await request(port, `/api/no-route?token=${secret}`);
  const broken = await request(port, `/api/broken?token=${secret}`);
  assert.equal(broken.status, 502);
  const logResult = spawnSync("docker", ["logs", container], {
    encoding: "utf8",
    windowsHide: true,
  });
  assert.equal(logResult.status, 0);
  const logs = logResult.stdout + logResult.stderr;
  assert.ok(
    !logs.includes(secret),
    "Nginx 日志包含路径、query 或 Referer 凭据",
  );
  assert.match(logs, /"status":404/);
  const entries = logs
    .split(/\r?\n/)
    .filter((line) => line.startsWith("{"))
    .map((line) => JSON.parse(line));
  for (const status of [200, 404, 502]) {
    assert.ok(
      entries.some(
        (entry) =>
          entry.status === status &&
          entry.upstream_status
            ?.split(", ")
            .every((value) => value === String(status)) &&
          /^\d+\.\d+(?:, \d+\.\d+)*$/.test(entry.upstream_response_time) &&
          Number.isFinite(entry.duration),
      ),
      `安全日志必须保留上游 ${status} 的状态和耗时：${JSON.stringify(entries.filter((entry) => entry.status === status))}`,
    );
  }
  stdout.write(
    "Nginx runtime: gzip/identity/Vary/PNG/precompressed/16MiB-file/17MiB-boundary/content-length+chunked/upstream-diagnostics/log-redaction passed\n",
  );
} finally {
  upstream.closeAllConnections();
  await new Promise((resolveClose) => upstream.close(resolveClose));
  if (container && /^[a-f0-9]{64}$/.test(container))
    docker("rm", "-f", container);
  assert.ok(
    resolve(directory).startsWith(resolve(tmpdir()) + "\\yang-nginx-check-") ||
      resolve(directory).startsWith(resolve(tmpdir()) + "/yang-nginx-check-"),
  );
  await rm(directory, { recursive: true });
}
