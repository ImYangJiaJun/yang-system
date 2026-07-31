import { createReadStream } from "node:fs";
import { stat } from "node:fs/promises";
import { createServer, request as httpRequest } from "node:http";
import { extname, isAbsolute, relative, resolve, sep } from "node:path";
import process, { env, stderr, stdout } from "node:process";
import { URL } from "node:url";

import {
  cacheControl,
  deploymentHeaders,
} from "../deploy/deployment-contract.mjs";

const host = "127.0.0.1";
const port = positivePort(
  env.YANG_PRODUCTION_E2E_FRONTEND_PORT,
  "YANG_PRODUCTION_E2E_FRONTEND_PORT",
  5300,
);
const backendPort = positivePort(
  env.YANG_PRODUCTION_E2E_BACKEND_PORT,
  "YANG_PRODUCTION_E2E_BACKEND_PORT",
  18300,
);
const buildRoot = resolve(env.YANG_PRODUCTION_BUILD_ROOT || "dist/spa");
const indexPath = resolve(buildRoot, "index.html");
const backendOrigin = `http://127.0.0.1:${backendPort}`;
const proxyPrefixes = ["/api", "/.well-known", "/health"];
const contentTypes = new Map([
  [".css", "text/css; charset=utf-8"],
  [".gif", "image/gif"],
  [".html", "text/html; charset=utf-8"],
  [".ico", "image/x-icon"],
  [".jpeg", "image/jpeg"],
  [".jpg", "image/jpeg"],
  [".js", "text/javascript; charset=utf-8"],
  [".json", "application/json; charset=utf-8"],
  [".png", "image/png"],
  [".svg", "image/svg+xml"],
  [".txt", "text/plain; charset=utf-8"],
  [".webp", "image/webp"],
  [".woff", "font/woff"],
  [".woff2", "font/woff2"],
]);

await requireFile(indexPath);

const server = createServer(async (request, response) => {
  try {
    const target = request.url || "/";
    const pathname = new URL(target, `http://${host}:${port}`).pathname;
    if (proxyPrefixes.some((prefix) => pathMatches(pathname, prefix))) {
      proxyRequest(request, response, target);
      return;
    }
    await serveStatic(request.method || "GET", target, response);
  } catch (error) {
    if (!response.headersSent) {
      sendText(response, 500, "Internal Server Error");
    } else {
      response.destroy();
    }
    stderr.write(
      `production build server error: ${
        error instanceof Error ? error.stack || error.message : String(error)
      }\n`,
    );
  }
});

server.on("clientError", (_error, socket) => {
  socket.end("HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n");
});

server.listen(port, host, () => {
  stdout.write(
    `production build server listening on http://${host}:${port}, root=${buildRoot}, backend=${backendOrigin}\n`,
  );
});

for (const signal of ["SIGINT", "SIGTERM"]) {
  process.once(signal, () => {
    server.close((error) => {
      if (error) {
        stderr.write(`production build server shutdown failed: ${error}\n`);
        process.exitCode = 1;
      }
    });
  });
}

function positivePort(raw, name, fallback) {
  const value = Number(raw || fallback);
  if (!Number.isInteger(value) || value < 1 || value > 65_535) {
    throw new Error(`${name} 必须是 1-65535 的整数`);
  }
  return value;
}

function pathMatches(pathname, prefix) {
  return pathname === prefix || pathname.startsWith(`${prefix}/`);
}

async function requireFile(path) {
  const metadata = await stat(path);
  if (!metadata.isFile()) {
    throw new Error(`生产构建入口不存在：${path}`);
  }
}

async function serveStatic(method, target, response) {
  if (method !== "GET" && method !== "HEAD") {
    response.setHeader("Allow", "GET, HEAD");
    sendText(response, 405, "Method Not Allowed", method);
    return;
  }
  const rawPath = target.split(/[?#]/, 1)[0] || "/";
  let decodedRawPath;
  try {
    decodedRawPath = decodeURIComponent(rawPath);
  } catch {
    sendText(response, 400, "Bad Request", method);
    return;
  }
  if (
    decodedRawPath.includes("\0") ||
    decodedRawPath
      .split(/[\\/]/)
      .some((segment) => segment === "." || segment === "..")
  ) {
    sendText(response, 400, "Bad Request", method);
    return;
  }

  const pathname = new URL(target, `http://${host}:${port}`).pathname;
  const relativePath = pathname.replace(/^\/+/, "");
  const candidate = resolve(buildRoot, relativePath);
  if (!isInside(buildRoot, candidate)) {
    sendText(response, 400, "Bad Request", method);
    return;
  }

  if (relativePath) {
    const metadata = await stat(candidate).catch(() => undefined);
    if (metadata?.isFile()) {
      streamFile(
        response,
        candidate,
        method,
        relativePath.startsWith("assets/")
          ? cacheControl.immutableAsset
          : cacheControl.html,
      );
      return;
    }
  }

  if (pathname.startsWith("/assets/")) {
    sendText(response, 404, "Not Found", method);
    return;
  }

  response.setHeader("X-Yang-SPA-Fallback", "index.html");
  streamFile(response, indexPath, method, cacheControl.html);
}

function isInside(root, candidate) {
  const path = relative(root, candidate);
  return (
    path === "" ||
    (!path.startsWith(`..${sep}`) && path !== ".." && !isAbsolute(path))
  );
}

function streamFile(response, path, method, cachePolicy) {
  response.statusCode = 200;
  response.setHeader(
    "Content-Type",
    contentTypes.get(extname(path).toLowerCase()) || "application/octet-stream",
  );
  response.setHeader("Cache-Control", cachePolicy);
  applyDeploymentHeaders(response);
  if (method === "HEAD") {
    response.end();
    return;
  }
  const stream = createReadStream(path);
  stream.on("error", (error) => {
    if (!response.headersSent) {
      sendText(response, 500, "Internal Server Error");
    } else {
      response.destroy(error);
    }
  });
  stream.pipe(response);
}

function sendText(response, status, body, method = "GET") {
  response.statusCode = status;
  response.setHeader("Content-Type", "text/plain; charset=utf-8");
  response.setHeader("Cache-Control", "no-store");
  applyDeploymentHeaders(response);
  response.end(method === "HEAD" ? undefined : body);
}

function applyDeploymentHeaders(response) {
  for (const [name, value] of Object.entries(deploymentHeaders)) {
    response.setHeader(name, value);
  }
}

function proxyRequest(clientRequest, clientResponse, target) {
  const upstream = new URL(target, backendOrigin);
  const headers = { ...clientRequest.headers, host: upstream.host };
  delete headers.connection;
  const upstreamRequest = httpRequest(
    upstream,
    {
      method: clientRequest.method,
      headers,
    },
    (upstreamResponse) => {
      const responseHeaders = { ...upstreamResponse.headers };
      delete responseHeaders.connection;
      clientResponse.statusCode = upstreamResponse.statusCode || 502;
      for (const [name, value] of Object.entries(responseHeaders)) {
        if (value !== undefined) {
          clientResponse.setHeader(name, value);
        }
      }
      clientResponse.setHeader("Cache-Control", cacheControl.html);
      applyDeploymentHeaders(clientResponse);
      upstreamResponse.pipe(clientResponse);
    },
  );
  upstreamRequest.on("error", (error) => {
    if (!clientResponse.headersSent) {
      sendText(clientResponse, 502, "Bad Gateway");
    } else {
      clientResponse.destroy(error);
    }
  });
  clientRequest.on("aborted", () => upstreamRequest.destroy());
  clientRequest.pipe(upstreamRequest);
}
