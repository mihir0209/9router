#!/usr/bin/env node

const fs = require("fs");
const http = require("http");
const https = require("https");
const net = require("net");
const os = require("os");
const path = require("path");
const tls = require("tls");
const forge = require("node-forge");

const host = process.env.CLAUDE_DEBUG_PROXY_HOST || "127.0.0.1";
const port = Number(process.env.CLAUDE_DEBUG_PROXY_PORT || 20129);
const outputDir = path.join(os.tmpdir(), "9router-claude-header-debug");
const caCertPath = path.join(outputDir, "ca.crt");
const caKeyPath = path.join(outputDir, "ca.key");
const capturePath = path.join(outputDir, "requests.log");
const pidPath = path.join(outputDir, "proxy.pid");
const sensitiveHeaders = new Set([
  "authorization",
  "cookie",
  "proxy-authorization",
  "x-api-key",
]);

fs.mkdirSync(outputDir, { recursive: true, mode: 0o700 });

function createCertificateAuthority() {
  if (fs.existsSync(caCertPath) && fs.existsSync(caKeyPath)) {
    return {
      cert: forge.pki.certificateFromPem(fs.readFileSync(caCertPath, "utf8")),
      key: forge.pki.privateKeyFromPem(fs.readFileSync(caKeyPath, "utf8")),
    };
  }

  const keys = forge.pki.rsa.generateKeyPair(2048);
  const cert = forge.pki.createCertificate();
  cert.publicKey = keys.publicKey;
  cert.serialNumber = `01${Date.now().toString(16)}`;
  cert.validity.notBefore = new Date(Date.now() - 60_000);
  cert.validity.notAfter = new Date();
  cert.validity.notAfter.setFullYear(cert.validity.notBefore.getFullYear() + 1);
  const attributes = [
    { name: "commonName", value: "9Router Claude Header Debug CA" },
    { name: "organizationName", value: "9Router Debug" },
  ];
  cert.setSubject(attributes);
  cert.setIssuer(attributes);
  cert.setExtensions([
    { name: "basicConstraints", cA: true, critical: true },
    { name: "keyUsage", keyCertSign: true, cRLSign: true, critical: true },
    { name: "subjectKeyIdentifier" },
  ]);
  cert.sign(keys.privateKey, forge.md.sha256.create());

  fs.writeFileSync(caCertPath, forge.pki.certificateToPem(cert), { mode: 0o600 });
  fs.writeFileSync(caKeyPath, forge.pki.privateKeyToPem(keys.privateKey), { mode: 0o600 });
  return { cert, key: keys.privateKey };
}

const ca = createCertificateAuthority();
const certificateCache = new Map();

function getSecureContext(servername) {
  const name = servername.replace(/^\[|\]$/g, "");
  if (certificateCache.has(name)) return certificateCache.get(name);

  const keys = forge.pki.rsa.generateKeyPair(2048);
  const cert = forge.pki.createCertificate();
  cert.publicKey = keys.publicKey;
  cert.serialNumber = `${Date.now().toString(16)}${Math.floor(Math.random() * 0xffff).toString(16)}`;
  cert.validity.notBefore = new Date(Date.now() - 60_000);
  cert.validity.notAfter = new Date(Date.now() + 24 * 60 * 60 * 1000);
  cert.setSubject([{ name: "commonName", value: name }]);
  cert.setIssuer(ca.cert.subject.attributes);
  cert.setExtensions([
    { name: "basicConstraints", cA: false },
    { name: "keyUsage", digitalSignature: true, keyEncipherment: true },
    { name: "extKeyUsage", serverAuth: true },
    { name: "subjectAltName", altNames: net.isIP(name)
      ? [{ type: 7, ip: name }]
      : [{ type: 2, value: name }] },
  ]);
  cert.sign(ca.key, forge.md.sha256.create());

  const context = tls.createSecureContext({
    key: forge.pki.privateKeyToPem(keys.privateKey),
    cert: forge.pki.certificateToPem(cert),
  });
  certificateCache.set(name, context);
  return context;
}

function appendCapture(req, body, target) {
  const headers = Object.fromEntries(Object.entries(req.headers).map(([name, value]) => [
    name,
    sensitiveHeaders.has(name.toLowerCase()) ? "[REDACTED]" : value,
  ]));
  const record = {
    capturedAt: new Date().toISOString(),
    method: req.method,
    url: target.href,
    httpVersion: req.httpVersion,
    headers,
    body: body.toString("utf8"),
  };
  fs.appendFileSync(capturePath, `${JSON.stringify(record, null, 2)}\n\n`, { mode: 0o600 });
  console.log(`[capture] ${req.method} ${target.href}`);
}

function collectRequest(req) {
  return new Promise((resolve, reject) => {
    const chunks = [];
    req.on("data", (chunk) => chunks.push(chunk));
    req.on("end", () => resolve(Buffer.concat(chunks)));
    req.on("error", reject);
  });
}

async function forwardRequest(req, res, defaultProtocol = "http:") {
  try {
    const body = await collectRequest(req);
    const authority = req.headers.host;
    if (!authority) throw new Error("Request has no Host header");
    const target = new URL(req.url, `${defaultProtocol}//${authority}`);
    appendCapture(req, body, target);

    const headers = { ...req.headers };
    delete headers["proxy-authorization"];
    delete headers["proxy-connection"];

    const transport = target.protocol === "https:" ? https : http;
    const upstream = transport.request(target, {
      method: req.method,
      headers,
    }, (upstreamResponse) => {
      res.writeHead(upstreamResponse.statusCode || 502, upstreamResponse.headers);
      upstreamResponse.pipe(res);
    });
    upstream.on("error", (error) => {
      if (!res.headersSent) res.writeHead(502, { "content-type": "text/plain" });
      res.end(`Debug proxy upstream error: ${error.message}`);
    });
    upstream.end(body);
  } catch (error) {
    if (!res.headersSent) res.writeHead(500, { "content-type": "text/plain" });
    res.end(`Debug proxy error: ${error.message}`);
  }
}

const decryptedHttpServer = http.createServer((req, res) => forwardRequest(req, res, "https:"));
const proxyServer = http.createServer((req, res) => forwardRequest(req, res, "http:"));

proxyServer.on("connect", (req, socket, head) => {
  const authority = new URL(`https://${req.url}`);
  socket.write("HTTP/1.1 200 Connection Established\r\nProxy-Agent: 9Router-Debug\r\n\r\n");

  const tlsSocket = new tls.TLSSocket(socket, {
    isServer: true,
    secureContext: getSecureContext(authority.hostname),
    ALPNProtocols: ["http/1.1"],
  });
  tlsSocket.on("error", (error) => console.error(`[tls] ${error.message}`));
  if (head.length > 0) tlsSocket.unshift(head);
  decryptedHttpServer.emit("connection", tlsSocket);
});

proxyServer.on("clientError", (error, socket) => {
  console.error(`[client] ${error.message}`);
  socket.end("HTTP/1.1 400 Bad Request\r\n\r\n");
});

proxyServer.listen(port, host, () => {
  fs.writeFileSync(pidPath, `${process.pid}\n`, { mode: 0o600 });
  console.log(`Claude header debug proxy listening at http://${host}:${port}`);
  console.log(`CA certificate: ${caCertPath}`);
  console.log(`Request capture: ${capturePath}`);
});

function shutdown() {
  try { fs.unlinkSync(pidPath); } catch {}
  proxyServer.close(() => process.exit(0));
}

process.on("SIGINT", shutdown);
process.on("SIGTERM", shutdown);
