#!/usr/bin/env node
/**
 * AST-based parity auditor between Node.js Next.js API routes (`src/app/api/**`)
 * and Rust Axum routes (`rust/app/src/`).
 *
 * Outputs `scripts/ast_parity_report.json` and prints a concise summary.
 */

import fs from "node:fs";
import path from "node:path";
import { execSync } from "node:child_process";
import { createRequire } from "node:module";

const HTTP_METHODS = new Set(["GET", "POST", "PUT", "PATCH", "DELETE", "OPTIONS", "HEAD"]);
const BODY_IGNORED_PROPS = new Set([
  "hasOwnProperty",
  "toString",
  "valueOf",
  "constructor",
  "length",
  "keys",
  "values",
  "entries",
  "map",
  "filter",
  "forEach",
  "some",
  "every",
  "find",
  "trim",
]);

function resolveBabelParser() {
  const require = createRequire(import.meta.url);
  const candidates = [];
  try {
    candidates.push(require.resolve("@babel/parser"));
  } catch {
    // Ignore local miss; try git common dir for worktrees.
  }
  try {
    const commonDir = execSync("git rev-parse --git-common-dir", {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
    }).trim();
    if (commonDir) {
      const mainRoot = path.dirname(path.resolve(commonDir));
      candidates.push(path.join(mainRoot, "node_modules", "@babel", "parser", "lib", "index.js"));
    }
  } catch {
    // Not a git repo or git unavailable.
  }
  for (const candidate of candidates) {
    if (fs.existsSync(candidate)) {
      return require(candidate);
    }
  }
  throw new Error("Unable to locate @babel/parser in node_modules or git common root.");
}

const babelParser = resolveBabelParser();

function parseAst(code, filename) {
  return babelParser.parse(code, {
    sourceType: "module",
    sourceFilename: filename,
    plugins: ["typescript", "jsx", "importAttributes", "decorators-legacy"],
  });
}

/** Generic AST walker visiting every node with (node, parent, key). */
function walkAst(node, visitor, parent = null, key = null) {
  if (!node || typeof node !== "object") return;
  if (Array.isArray(node)) {
    for (let i = 0; i < node.length; i++) {
      walkAst(node[i], visitor, parent, key);
    }
    return;
  }
  if (typeof node.type !== "string") return;
  visitor(node, parent, key);
  for (const prop of Object.keys(node)) {
    if (prop === "loc" || prop === "start" || prop === "end" || prop === "extra") continue;
    const child = node[prop];
    if (child && typeof child === "object") {
      walkAst(child, visitor, node, prop);
    }
  }
}

function walkFiles(dir, predicate, out = []) {
  if (!fs.existsSync(dir)) return out;
  const entries = fs.readdirSync(dir, { withFileTypes: true });
  entries.sort((a, b) => a.name.localeCompare(b.name));
  for (const entry of entries) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      walkFiles(full, predicate, out);
    } else if (entry.isFile() && predicate(full, entry.name)) {
      out.push(full);
    }
  }
  return out;
}

/** Convert Next.js App Router path segments to Axum-style route template. */
function fileToNextUrlPath(repoRoot, filePath) {
  const apiRoot = path.join(repoRoot, "src", "app");
  const relDir = path.relative(apiRoot, path.dirname(filePath));
  const segments = relDir.split(path.sep).filter(Boolean);
  const converted = segments.map((seg) => {
    const optCatchAll = seg.match(/^\[\[\.\.\.(.+)\]\]$/);
    if (optCatchAll) return `{*${optCatchAll[1]}}`;
    const catchAll = seg.match(/^\[\.\.\.(.+)\]$/);
    if (catchAll) return `{*${catchAll[1]}}`;
    const dyn = seg.match(/^\[(.+)\]$/);
    if (dyn) return `{${dyn[1]}}`;
    return seg;
  });
  return "/" + converted.join("/");
}

function normalizeImportSource(source, filePath, repoRoot) {
  if (source.startsWith("@/")) {
    return "src/" + source.slice(2);
  }
  if (source.startsWith("./") || source.startsWith("../")) {
    const resolved = path.resolve(path.dirname(filePath), source);
    return path.relative(repoRoot, resolved).split(path.sep).join("/");
  }
  return source;
}

function getPropName(node) {
  if (!node) return null;
  if (node.type === "Identifier") return node.name;
  if (node.type === "StringLiteral") return node.value;
  return null;
}

function collectObjectPatternKeys(pattern, outSet) {
  if (!pattern || pattern.type !== "ObjectPattern") return;
  for (const prop of pattern.properties || []) {
    if (prop.type === "ObjectProperty") {
      const k = getPropName(prop.key);
      if (k) outSet.add(k);
    }
  }
}

function extractStringLiteral(node) {
  if (!node) return null;
  if (node.type === "StringLiteral") return node.value;
  if (node.type === "TemplateLiteral" && node.expressions.length === 0) {
    return node.quasis.map((q) => q.value.cooked ?? q.value.raw).join("");
  }
  return null;
}

function unwrapAsyncChain(expr) {
  let cur = expr;
  while (cur) {
    if (cur.type === "AwaitExpression") {
      cur = cur.argument;
      continue;
    }
    if (
      cur.type === "CallExpression" &&
      (cur.callee.type === "MemberExpression" || cur.callee.type === "OptionalMemberExpression") &&
      !cur.callee.computed &&
      cur.callee.property.type === "Identifier" &&
      (cur.callee.property.name === "catch" || cur.callee.property.name === "finally")
    ) {
      cur = cur.callee.object;
      continue;
    }
    break;
  }
  return cur;
}

function getRequestParseKind(expr, reqParamNames) {
  const inner = unwrapAsyncChain(expr);
  if (!inner || inner.type !== "CallExpression") return null;

  // readJsonLimited(request, ...)
  if (inner.callee.type === "Identifier" && inner.callee.name === "readJsonLimited") {
    return "json";
  }

  // req.json() / request.json() / req.formData()
  if (
    (inner.callee.type === "MemberExpression" || inner.callee.type === "OptionalMemberExpression") &&
    !inner.callee.computed &&
    inner.callee.property.type === "Identifier" &&
    (inner.callee.property.name === "json" || inner.callee.property.name === "formData")
  ) {
    const obj = inner.callee.object;
    if (obj.type === "Identifier") {
      if (reqParamNames.has(obj.name) || /^(req|request|_req|_request|newRequest|clonedReq)$/i.test(obj.name)) {
        return inner.callee.property.name;
      }
    }
  }
  return null;
}

function isRequestCloneCall(expr, reqParamNames) {
  if (!expr || expr.type !== "CallExpression") return false;
  if (
    (expr.callee.type === "MemberExpression" || expr.callee.type === "OptionalMemberExpression") &&
    !expr.callee.computed &&
    expr.callee.property.type === "Identifier" &&
    expr.callee.property.name === "clone" &&
    expr.callee.object.type === "Identifier"
  ) {
    return reqParamNames.has(expr.callee.object.name);
  }
  return false;
}

function isSearchParamsExpr(expr) {
  if (!expr) return false;
  // new URL(req.url).searchParams
  if (
    expr.type === "MemberExpression" &&
    !expr.computed &&
    expr.property.type === "Identifier" &&
    expr.property.name === "searchParams"
  ) {
    return true;
  }
  // searchParams itself
  if (expr.type === "Identifier" && /searchParams/i.test(expr.name)) {
    return true;
  }
  return false;
}

function collectParamNamesFromFn(fnNode) {
  const names = new Set();
  if (!fnNode || !fnNode.params || fnNode.params.length === 0) return names;
  const first = fnNode.params[0];
  if (first.type === "Identifier") names.add(first.name);
  return names;
}

function analyzeNodeFileAst(filePath, repoRoot, visited = new Set()) {
  const absPath = path.resolve(filePath);
  if (visited.has(absPath) || !fs.existsSync(absPath)) {
    return null;
  }
  visited.add(absPath);

  const code = fs.readFileSync(absPath, "utf8");
  const ast = parseAst(code, absPath);

  const imports = new Set();
  const exportedMethods = new Map(); // method -> { kind, reexportedFrom?, handlerNode? }
  const localDecls = new Map(); // name -> declaration/init node
  const nextRouteConfig = {};
  const queryParams = new Set();
  const bodyFields = new Set();
  const bodyParsingKinds = new Set();
  const authSignals = new Set();
  const dbModules = new Set();
  const dbOperations = new Set();
  const reexportTargets = [];

  // Index top-level declarations so `export const GET = POST;` or `export const GET = proxy;` resolves cleanly.
  for (const stmt of ast.program.body) {
    if (stmt.type === "FunctionDeclaration" && stmt.id) {
      localDecls.set(stmt.id.name, stmt);
    } else if (stmt.type === "VariableDeclaration") {
      for (const d of stmt.declarations) {
        if (d.id.type === "Identifier" && d.init) {
          localDecls.set(d.id.name, d.init);
        }
      }
    } else if (stmt.type === "ExportNamedDeclaration" && stmt.declaration) {
      if (stmt.declaration.type === "FunctionDeclaration" && stmt.declaration.id) {
        localDecls.set(stmt.declaration.id.name, stmt.declaration);
      } else if (stmt.declaration.type === "VariableDeclaration") {
        for (const d of stmt.declaration.declarations) {
          if (d.id.type === "Identifier" && d.init) {
            localDecls.set(d.id.name, d.init);
          }
        }
      }
    }
  }

  // Extract top-level imports & exports.
  for (const stmt of ast.program.body) {
    if (stmt.type === "ImportDeclaration" && stmt.source?.value) {
      imports.add(normalizeImportSource(stmt.source.value, absPath, repoRoot));
    } else if (stmt.type === "ExportAllDeclaration" && stmt.source?.value) {
      imports.add(normalizeImportSource(stmt.source.value, absPath, repoRoot));
    } else if (stmt.type === "ExportNamedDeclaration") {
      if (stmt.source?.value) {
        const norm = normalizeImportSource(stmt.source.value, absPath, repoRoot);
        imports.add(norm);
        const targetAbs = resolveLocalModuleFile(absPath, stmt.source.value);
        if (targetAbs) reexportTargets.push(targetAbs);
        for (const spec of stmt.specifiers || []) {
          const expName = getPropName(spec.exported);
          if (expName && HTTP_METHODS.has(expName)) {
            exportedMethods.set(expName, {
              kind: "reexport",
              reexportedFrom: norm,
            });
          }
        }
      } else if (stmt.declaration) {
        if (stmt.declaration.type === "FunctionDeclaration" && stmt.declaration.id) {
          const name = stmt.declaration.id.name;
          if (HTTP_METHODS.has(name)) {
            exportedMethods.set(name, { kind: "function", handlerNode: stmt.declaration });
          }
        } else if (stmt.declaration.type === "VariableDeclaration") {
          for (const decl of stmt.declaration.declarations) {
            if (decl.id.type !== "Identifier") continue;
            const name = decl.id.name;
            if (HTTP_METHODS.has(name)) {
              let targetNode = decl.init;
              if (targetNode?.type === "Identifier" && localDecls.has(targetNode.name)) {
                targetNode = localDecls.get(targetNode.name);
              }
              exportedMethods.set(name, { kind: "const", handlerNode: targetNode });
            } else if (["dynamic", "runtime", "maxDuration", "revalidate"].includes(name)) {
              const lit = extractStringLiteral(decl.init);
              if (lit !== null) nextRouteConfig[name] = lit;
              else if (decl.init?.type === "NumericLiteral") nextRouteConfig[name] = decl.init.value;
            }
          }
        }
      } else if (stmt.specifiers) {
        for (const spec of stmt.specifiers) {
          const expName = getPropName(spec.exported);
          const localName = getPropName(spec.local);
          if (expName && HTTP_METHODS.has(expName)) {
            exportedMethods.set(expName, {
              kind: "named_specifier",
              handlerNode: localName ? localDecls.get(localName) : null,
            });
          }
        }
      }
    }
  }

  // Also collect dynamic imports and require() anywhere in the AST.
  const reqParamNames = new Set(["req", "request", "_req", "_request"]);
  for (const [, meta] of exportedMethods) {
    if (meta.handlerNode) {
      for (const p of collectParamNamesFromFn(meta.handlerNode)) reqParamNames.add(p);
    }
  }

  const searchParamsVars = new Set(["searchParams"]);
  const bodyVars = new Set();
  const formDataVars = new Set();

  walkAst(ast.program, (node) => {
    // Dynamic import("...") or require("...")
    if (node.type === "CallExpression") {
      if (node.callee.type === "Import" && node.arguments[0]?.type === "StringLiteral") {
        imports.add(normalizeImportSource(node.arguments[0].value, absPath, repoRoot));
      } else if (
        node.callee.type === "Identifier" &&
        node.callee.name === "require" &&
        node.arguments[0]?.type === "StringLiteral"
      ) {
        imports.add(normalizeImportSource(node.arguments[0].value, absPath, repoRoot));
      }
    }

    // Track `const { searchParams } = new URL(...)` or `const sp = ...searchParams`
    if (node.type === "VariableDeclarator" && node.init) {
      if (isRequestCloneCall(node.init, reqParamNames) && node.id.type === "Identifier") {
        reqParamNames.add(node.id.name);
      }
      if (isSearchParamsExpr(node.init)) {
        if (node.id.type === "Identifier") {
          searchParamsVars.add(node.id.name);
        }
      }
      if (
        node.init.type === "NewExpression" &&
        node.init.callee.type === "Identifier" &&
        node.init.callee.name === "URL" &&
        node.id.type === "ObjectPattern"
      ) {
        for (const prop of node.id.properties) {
          if (prop.type === "ObjectProperty" && getPropName(prop.key) === "searchParams") {
            if (prop.value.type === "Identifier") searchParamsVars.add(prop.value.name);
          }
        }
      }
      // Track `const body = await request.json()` or `const formData = await request.formData()`
      const parseKind = getRequestParseKind(node.init, reqParamNames);
      if (parseKind === "formData") {
        bodyParsingKinds.add("formData");
        if (node.id.type === "Identifier") {
          formDataVars.add(node.id.name);
        }
      } else if (parseKind === "json") {
        bodyParsingKinds.add("json");
        if (node.id.type === "Identifier") {
          bodyVars.add(node.id.name);
        } else if (node.id.type === "ObjectPattern") {
          collectObjectPatternKeys(node.id, bodyFields);
        }
      }
      // Track destructuring from an existing bodyVar: `const { name, alias } = body`
      if (
        node.init.type === "Identifier" &&
        bodyVars.has(node.init.name) &&
        node.id.type === "ObjectPattern"
      ) {
        collectObjectPatternKeys(node.id, bodyFields);
      }
    }

    // Track `searchParams.get("foo")`, `formData.get("bar")`, `.getAll(...)`, `.has(...)`
    if (
      node.type === "CallExpression" &&
      (node.callee.type === "MemberExpression" || node.callee.type === "OptionalMemberExpression")
    ) {
      const propName = getPropName(node.callee.property);
      if (propName && ["get", "getAll", "has"].includes(propName)) {
        const obj = node.callee.object;
        const isSpCall =
          (obj.type === "Identifier" && searchParamsVars.has(obj.name)) ||
          ((obj.type === "MemberExpression" || obj.type === "OptionalMemberExpression") &&
            getPropName(obj.property) === "searchParams");
        if (isSpCall) {
          const arg = extractStringLiteral(node.arguments[0]);
          if (arg) queryParams.add(arg);
        } else if (obj.type === "Identifier" && formDataVars.has(obj.name)) {
          const arg = extractStringLiteral(node.arguments[0]);
          if (arg) bodyFields.add(arg);
        }
      }
      // Helper pattern like `getQueryParam(url, "profile")`
      if (
        node.callee.type === "Identifier" &&
        /queryParam/i.test(node.callee.name) &&
        node.arguments.length >= 2
      ) {
        const arg = extractStringLiteral(node.arguments[1]);
        if (arg) queryParams.add(arg);
      }
    }

    // Track `body.foo`, `body?.foo`, `body["foo"]`
    if (
      (node.type === "MemberExpression" || node.type === "OptionalMemberExpression") &&
      node.object.type === "Identifier" &&
      bodyVars.has(node.object.name)
    ) {
      const prop = node.computed ? extractStringLiteral(node.property) : getPropName(node.property);
      if (prop && !BODY_IGNORED_PROPS.has(prop)) {
        bodyFields.add(prop);
      }
    }

    // Detect auth signals in identifiers and string literals
    if (node.type === "Identifier") {
      if (
        /^(isValidApiKey|validateAndEnforceApiKey|authenticateModelsRequest|isAuthenticated|isLocalRequest|requireAdmin|hasValidAdminApiToken|verifyDashboardAuthToken|verifyAuth|getSession|cookies|jwtVerify|byokUpstreamErrorResponse|credentialsErrorResponse)$/.test(
          node.name
        )
      ) {
        authSignals.add(node.name);
      }
    }
    if (node.type === "StringLiteral") {
      const v = node.value.toLowerCase();
      if (v === "authorization" || v === "x-api-key" || v === "x-goog-api-key" || v === "x-admin-token" || v === "x-9r-cli-token") {
        authSignals.add(`header:${v}`);
      }
    }
  });

  // Categorize imports and DB expectations.
  const allImports = [...imports].sort();
  const libModules = allImports.filter((m) => m.startsWith("src/lib/"));
  const openSseModules = allImports.filter((m) => m.startsWith("open-sse/"));
  const modelsModules = allImports.filter(
    (m) => m === "src/models" || m.startsWith("src/models/") || m.startsWith("src/lib/localDb") || m.includes("Db")
  );
  const sseModules = allImports.filter((m) => m.startsWith("src/sse/"));
  const sharedModules = allImports.filter((m) => m.startsWith("src/shared/"));

  for (const m of modelsModules) dbModules.add(m);
  for (const imp of allImports) {
    if (imp.includes("db/") || imp.includes("sqlite") || imp.includes("postgres")) {
      dbModules.add(imp);
    }
    if (imp.includes("auth") || imp.includes("dashboardGuard")) {
      authSignals.add(`import:${imp}`);
    }
  }

  // Collect imported DB operation identifiers from DB/models modules.
  for (const stmt of ast.program.body) {
    if (stmt.type === "ImportDeclaration" && stmt.source?.value) {
      const norm = normalizeImportSource(stmt.source.value, absPath, repoRoot);
      if (
        norm === "src/models" ||
        norm.startsWith("src/models/") ||
        norm.startsWith("src/lib/localDb") ||
        norm.includes("Db") ||
        norm.startsWith("src/lib/usage") ||
        norm.startsWith("src/lib/db/")
      ) {
        for (const spec of stmt.specifiers) {
          if (spec.imported?.name) dbOperations.add(spec.imported.name);
        }
      }
      if (norm.includes("auth") || norm.includes("dashboardGuard") || norm.includes("validateApiKey")) {
        for (const spec of stmt.specifiers) {
          if (spec.imported?.name) authSignals.add(spec.imported.name);
        }
      }
    }
  }

  // If this route re-exports handlers from another local route (e.g. src/app/api/v1/route.js),
  // merge its metadata so body/query/auth/DB expectations are accurate.
  for (const target of reexportTargets) {
    const sub = analyzeNodeFileAst(target, repoRoot, visited);
    if (sub) {
      for (const q of sub.queryParams) queryParams.add(q);
      for (const b of sub.bodyFields) bodyFields.add(b);
      for (const a of sub.authSignals) authSignals.add(a);
      for (const d of sub.dbModules) dbModules.add(d);
      for (const op of sub.dbOperations) dbOperations.add(op);
    }
  }

  return {
    methods: [...exportedMethods.keys()],
    methodDetails: Object.fromEntries(
      [...exportedMethods.entries()].map(([m, v]) => [
        m,
        { kind: v.kind, ...(v.reexportedFrom ? { reexportedFrom: v.reexportedFrom } : {}) },
      ])
    ),
    nextRouteConfig,
    allImports,
    libModules,
    openSseModules,
    modelsModules,
    sseModules,
    sharedModules,
    queryParams: [...queryParams].sort(),
    bodyFields: [...bodyFields].sort(),
    bodyParsingKinds: [...bodyParsingKinds].sort(),
    authSignals: [...authSignals].sort(),
    dbModules: [...dbModules].sort(),
    dbOperations: [...dbOperations].sort(),
  };
}

function resolveLocalModuleFile(fromFile, spec) {
  if (!spec.startsWith("./") && !spec.startsWith("../")) return null;
  const base = path.resolve(path.dirname(fromFile), spec);
  const candidates = [
    base,
    `${base}.js`,
    `${base}.ts`,
    `${base}.mjs`,
    path.join(base, "index.js"),
    path.join(base, "index.ts"),
  ];
  return candidates.find((c) => fs.existsSync(c) && fs.statSync(c).isFile()) || null;
}

/** Parse `next.config.mjs` AST to extract `rewrites()` rules. */
function parseNextRewrites(repoRoot) {
  const cfgPath = path.join(repoRoot, "next.config.mjs");
  if (!fs.existsSync(cfgPath)) return [];
  const ast = parseAst(fs.readFileSync(cfgPath, "utf8"), cfgPath);
  const rewrites = [];
  walkAst(ast.program, (node) => {
    if (
      (node.type === "ObjectMethod" || node.type === "ObjectProperty") &&
      getPropName(node.key) === "rewrites"
    ) {
      walkAst(node, (inner) => {
        if (inner.type === "ObjectExpression") {
          let source = null;
          let destination = null;
          for (const p of inner.properties) {
            if (p.type !== "ObjectProperty") continue;
            const k = getPropName(p.key);
            if (k === "source") source = extractStringLiteral(p.value);
            if (k === "destination") destination = extractStringLiteral(p.value);
          }
          if (source && destination) {
            rewrites.push({ source, destination });
          }
        }
      });
    }
  });
  return rewrites;
}

/** Parse `src/dashboardGuard.js` AST to extract route auth policy buckets. */
function parseDashboardGuard(repoRoot) {
  const guardPath = path.join(repoRoot, "src", "dashboardGuard.js");
  const buckets = {
    PUBLIC_API_PATHS: [],
    PUBLIC_PREFIXES: [],
    ADMIN_API_PREFIXES: [],
    ALWAYS_PROTECTED: [],
    PROTECTED_API_PATHS: [],
    LOCAL_ONLY_PATHS: [],
  };
  if (!fs.existsSync(guardPath)) return buckets;
  const ast = parseAst(fs.readFileSync(guardPath, "utf8"), guardPath);
  walkAst(ast.program, (node) => {
    if (
      node.type === "VariableDeclarator" &&
      node.id.type === "Identifier" &&
      Object.hasOwn(buckets, node.id.name) &&
      node.init?.type === "ArrayExpression"
    ) {
      for (const elem of node.init.elements) {
        const s = extractStringLiteral(elem);
        if (s) buckets[node.id.name].push(s);
      }
    }
  });
  return buckets;
}

function classifyGuardPolicy(urlPath, guardBuckets) {
  const matchesPrefix = (list) =>
    list.some((p) => urlPath === p || urlPath.startsWith(p.endsWith("/") ? p : p + "/"));
  return {
    isPublicApiPath: matchesPrefix(guardBuckets.PUBLIC_API_PATHS),
    isPublicPrefix: matchesPrefix(guardBuckets.PUBLIC_PREFIXES),
    isAdminPrefix: matchesPrefix(guardBuckets.ADMIN_API_PREFIXES),
    isAlwaysProtected: matchesPrefix(guardBuckets.ALWAYS_PROTECTED),
    isProtectedPath: matchesPrefix(guardBuckets.PROTECTED_API_PATHS),
    isLocalOnly: matchesPrefix(guardBuckets.LOCAL_ONLY_PATHS),
  };
}

/**
 * Strip comments, strings (optionally preserving string spans), and `#[cfg(test)]`
 * modules from Rust source so test routers are never treated as production routes.
 */
function stripRustCfgTests(source) {
  const lines = source.split("\n");
  const out = [];
  let inCfgTest = false;
  let sawCfgTestAttr = false;
  let braceDepth = 0;
  let seenOpeningBrace = false;

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    if (!inCfgTest) {
      if (/^\s*#\[\s*cfg\s*\(\s*test\s*\)\s*\]/.test(line)) {
        sawCfgTestAttr = true;
        out.push("");
        continue;
      }
      if (sawCfgTestAttr) {
        inCfgTest = true;
        sawCfgTestAttr = false;
        braceDepth = 0;
        seenOpeningBrace = false;
      }
    }

    if (inCfgTest) {
      for (const ch of line) {
        if (ch === "{") {
          braceDepth++;
          seenOpeningBrace = true;
        } else if (ch === "}") {
          braceDepth--;
        }
      }
      out.push("");
      if (seenOpeningBrace && braceDepth <= 0) {
        inCfgTest = false;
      }
      continue;
    }

    out.push(line);
  }
  return out.join("\n");
}

function skipWsAndComments(src, idx) {
  let i = idx;
  while (i < src.length) {
    if (/\s/.test(src[i])) {
      i++;
    } else if (src[i] === "/" && src[i + 1] === "/") {
      i += 2;
      while (i < src.length && src[i] !== "\n") i++;
    } else if (src[i] === "/" && src[i + 1] === "*") {
      i += 2;
      while (i < src.length && !(src[i] === "*" && src[i + 1] === "/")) i++;
      i += 2;
    } else {
      break;
    }
  }
  return i;
}

function readBalancedParen(src, openIdx) {
  let depth = 0;
  let i = openIdx;
  while (i < src.length) {
    const ch = src[i];
    if (ch === "/" && src[i + 1] === "/") {
      i += 2;
      while (i < src.length && src[i] !== "\n") i++;
      continue;
    }
    if (ch === "/" && src[i + 1] === "*") {
      i += 2;
      while (i < src.length && !(src[i] === "*" && src[i + 1] === "/")) i++;
      i += 2;
      continue;
    }
    if (ch === '"') {
      i++;
      while (i < src.length) {
        if (src[i] === "\\") i += 2;
        else if (src[i] === '"') {
          i++;
          break;
        } else i++;
      }
      continue;
    }
    if (ch === "(") depth++;
    else if (ch === ")") {
      depth--;
      if (depth === 0) return i;
    }
    i++;
  }
  return -1;
}

function splitTopLevelComma(expr) {
  let depthParen = 0;
  let depthBrace = 0;
  let depthBracket = 0;
  for (let i = 0; i < expr.length; i++) {
    const ch = expr[i];
    if (ch === '"') {
      i++;
      while (i < expr.length) {
        if (expr[i] === "\\") i += 2;
        else if (expr[i] === '"') break;
        else i++;
      }
      continue;
    }
    if (ch === "(") depthParen++;
    else if (ch === ")") depthParen--;
    else if (ch === "{") depthBrace++;
    else if (ch === "}") depthBrace--;
    else if (ch === "[") depthBracket++;
    else if (ch === "]") depthBracket--;
    else if (ch === "," && depthParen === 0 && depthBrace === 0 && depthBracket === 0) {
      return [expr.slice(0, i), expr.slice(i + 1)];
    }
  }
  return null;
}

function extractMethodsFromAxumExpr(expr) {
  const methods = [];
  const VERBS = new Set(["get", "post", "put", "patch", "delete", "options", "head", "any"]);
  let i = skipWsAndComments(expr, 0);
  while (i < expr.length) {
    if (methods.length === 0 && expr.startsWith("axum::routing::", i)) {
      i += "axum::routing::".length;
      i = skipWsAndComments(expr, i);
    } else if (expr[i] === ".") {
      i++;
      i = skipWsAndComments(expr, i);
      if (expr.startsWith("axum::routing::", i)) {
        i += "axum::routing::".length;
        i = skipWsAndComments(expr, i);
      }
    } else if (methods.length > 0) {
      break;
    }

    const identMatch = expr.slice(i).match(/^([a-zA-Z_][a-zA-Z0-9_]*)\s*\(/);
    if (!identMatch) break;
    const fnName = identMatch[1];
    const openIdx = expr.indexOf("(", i);
    const closeIdx = readBalancedParen(expr, openIdx);
    if (closeIdx === -1) break;
    if (VERBS.has(fnName)) {
      const handler = expr.slice(openIdx + 1, closeIdx).trim();
      methods.push({ method: fnName.toUpperCase(), handler });
    }
    i = skipWsAndComments(expr, closeIdx + 1);
  }
  return methods;
}

function parseRustRoutes(repoRoot) {
  const rustSrcDir = path.join(repoRoot, "rust", "app", "src");
  const rsFiles = walkFiles(rustSrcDir, (_full, name) => name.endsWith(".rs"));

  const handlerLocations = new Map(); // fn_name -> file:line
  const routeEntries = [];

  for (const file of rsFiles) {
    const rel = path.relative(repoRoot, file).split(path.sep).join("/");
    const raw = fs.readFileSync(file, "utf8");
    const stripped = stripRustCfgTests(raw);

    // Index async handler definitions across all Rust modules (routes.rs, responses.rs, v1beta.rs, media/*.rs, etc.).
    const lines = stripped.split("\n");
    for (let i = 0; i < lines.length; i++) {
      const hm = lines[i].match(/\bpub\s+(?:async\s+)?fn\s+([a-zA-Z0-9_]+)\s*\(/);
      if (hm) {
        handlerLocations.set(hm[1], `${rel}:${i + 1}`);
      }
    }

    // Parse `.route( "<path>", <method_router> )` calls.
    const routeCallRe = /\.route\s*\(/g;
    let rm;
    while ((rm = routeCallRe.exec(stripped)) !== null) {
      const openIdx = rm.index + rm[0].length - 1;
      const closeIdx = readBalancedParen(stripped, openIdx);
      if (closeIdx === -1) continue;
      const inner = stripped.slice(openIdx + 1, closeIdx);
      const split = splitTopLevelComma(inner);
      if (!split) continue;
      const pathPart = split[0].trim();
      const handlerPart = split[1].trim();
      const pathMatch = pathPart.match(/^"([^"]+)"$/);
      if (!pathMatch) continue;

      const routePath = pathMatch[1];
      const lineNum = stripped.slice(0, rm.index).split("\n").length;
      const parsedMethods = extractMethodsFromAxumExpr(handlerPart);
      routeEntries.push({
        path: routePath,
        file: rel,
        line: lineNum,
        methods: parsedMethods,
      });
    }
  }

  // Merge duplicate route entries if a path appears multiple times, and enrich with handler file:line.
  const byPath = new Map();
  for (const entry of routeEntries) {
    if (!byPath.has(entry.path)) {
      byPath.set(entry.path, {
        path: entry.path,
        registrations: [],
        methods: new Set(),
        handlers: {},
      });
    }
    const rec = byPath.get(entry.path);
    rec.registrations.push(`${entry.file}:${entry.line}`);
    for (const mh of entry.methods) {
      rec.methods.add(mh.method);
      const shortFn = mh.handler.split("::").pop();
      rec.handlers[mh.method] = {
        expr: mh.handler,
        definedAt: handlerLocations.get(shortFn) || null,
      };
    }
  }

  return {
    byPath,
    handlerCount: handlerLocations.size,
  };
}

function getNamespace(urlPath) {
  const parts = urlPath.replace(/^\/api\/?/, "").split("/").filter(Boolean);
  return parts[0] || "(root)";
}

function deriveNextRewriteAliases(urlPath, rewrites) {
  const aliases = new Set();
  for (const r of rewrites) {
    // Handle patterns like /v1/:path* -> /api/v1/:path*
    if (r.destination === "/api/v1/:path*" && urlPath.startsWith("/api/v1/")) {
      aliases.add("/v1/" + urlPath.slice("/api/v1/".length));
    } else if (r.destination === "/api/v1" && urlPath === "/api/v1") {
      aliases.add("/v1");
    } else if (r.destination === "/api/v1beta/:path*" && urlPath.startsWith("/api/v1beta/")) {
      aliases.add("/v1beta/" + urlPath.slice("/api/v1beta/".length));
    } else if (r.destination === "/api/v1beta" && urlPath === "/api/v1beta") {
      aliases.add("/v1beta");
    } else if (r.destination === urlPath && !r.source.includes(":path*")) {
      aliases.add(r.source);
    }
  }
  return [...aliases];
}

function findRustAncestorsAndDescendants(nodePath, rustPaths) {
  const ancestors = [];
  const descendants = [];
  for (const rp of rustPaths) {
    if (rp === nodePath) continue;
    if (nodePath.startsWith(rp + "/")) ancestors.push(rp);
    if (rp.startsWith(nodePath + "/")) descendants.push(rp);
  }
  return { ancestors: ancestors.sort(), descendants: descendants.sort() };
}

function main() {
  const repoRoot = process.cwd();
  const apiDir = path.join(repoRoot, "src", "app", "api");
  const routeFiles = walkFiles(
    apiDir,
    (_full, name) => name === "route.js" || name === "route.ts"
  );

  const rewrites = parseNextRewrites(repoRoot);
  const guardBuckets = parseDashboardGuard(repoRoot);
  const rustData = parseRustRoutes(repoRoot);
  const rustByPath = rustData.byPath;
  const allRustPaths = [...rustByPath.keys()].sort();

  // Track which Rust paths matched a Node route.
  const matchedRustPaths = new Set();
  const nodeRoutes = [];

  for (const file of routeFiles) {
    const relFile = path.relative(repoRoot, file).split(path.sep).join("/");
    const urlPath = fileToNextUrlPath(repoRoot, file);
    const namespace = getNamespace(urlPath);
    const rewriteAliases = deriveNextRewriteAliases(urlPath, rewrites);
    const astInfo = analyzeNodeFileAst(file, repoRoot);
    const guardPolicy = classifyGuardPolicy(urlPath, guardBuckets);

    const exactRust = rustByPath.get(urlPath) || null;
    const aliasRustMatches = rewriteAliases
      .map((a) => ({ alias: a, entry: rustByPath.get(a) || null }))
      .filter((x) => x.entry !== null);

    if (exactRust) matchedRustPaths.add(urlPath);
    for (const am of aliasRustMatches) matchedRustPaths.add(am.alias);

    const rustEntry = exactRust || (aliasRustMatches[0]?.entry ?? null);
    const rustMethods = rustEntry ? [...rustEntry.methods].sort() : [];
    const nodeMethods = [...astInfo.methods];

    let status;
    let partialReason = null;
    let missingMethods = [];
    let extraRustMethods = [];

    if (rustEntry) {
      missingMethods = nodeMethods.filter(
        (m) =>
          !rustEntry.methods.has(m) &&
          !rustEntry.methods.has("ANY") &&
          !(m === "HEAD" && rustEntry.methods.has("GET"))
      );
      extraRustMethods = rustMethods.filter((m) => !nodeMethods.includes(m));
      if (!exactRust && aliasRustMatches.length > 0) {
        status = "PARTIAL_METHODS";
        partialReason = "rewrite_alias_only_missing_api_prefix";
      } else if (missingMethods.length > 0) {
        status = "PARTIAL_METHODS";
        partialReason = "missing_verb";
      } else {
        status = "PARITY_OK";
      }
    } else {
      status = "MISSING";
      missingMethods = [...nodeMethods];
    }

    const { ancestors, descendants } = findRustAncestorsAndDescendants(urlPath, allRustPaths);

    nodeRoutes.push({
      file: relFile,
      path: urlPath,
      namespace,
      rewriteAliases,
      status,
      partialReason,
      nodeMethods,
      rustMethods,
      missingMethods,
      extraRustMethods,
      rustRegisteredPaths: [
        ...(exactRust ? [urlPath] : []),
        ...aliasRustMatches.map((a) => a.alias),
      ],
      rustRegistrations: rustEntry ? rustEntry.registrations : [],
      rustHandlers: rustEntry ? rustEntry.handlers : {},
      rustRelatedAncestors: ancestors,
      rustRelatedDescendants: descendants,
      nextRouteConfig: astInfo.nextRouteConfig,
      imports: {
        lib: astInfo.libModules,
        openSse: astInfo.openSseModules,
        models: astInfo.modelsModules,
        sse: astInfo.sseModules,
        shared: astInfo.sharedModules,
        all: astInfo.allImports,
      },
      queryParams: astInfo.queryParams,
      bodyFields: astInfo.bodyFields,
      bodyParsingKinds: astInfo.bodyParsingKinds,
      authExpectations: {
        guardPolicy,
        inlineSignals: astInfo.authSignals,
      },
      dbExpectations: {
        usesDb: astInfo.dbModules.length > 0 || astInfo.dbOperations.length > 0,
        modules: astInfo.dbModules,
        operations: astInfo.dbOperations,
      },
    });
  }

  // Compute namespace presence so we can highlight subtle/non-obvious MISSING routes
  // inside partially-implemented namespaces vs completely unported namespaces.
  const namespacesWithRust = new Set();
  for (const rp of allRustPaths) {
    if (rp.startsWith("/api/")) {
      namespacesWithRust.add(getNamespace(rp));
    }
  }

  for (const r of nodeRoutes) {
    r.namespaceHasRustRoutes = namespacesWithRust.has(r.namespace);
    r.isSubpathOrParentOfRustRoute =
      r.status === "MISSING" &&
      (r.rustRelatedAncestors.length > 0 || r.rustRelatedDescendants.length > 0);
    r.isNonObviousMissing =
      r.status === "MISSING" && (r.namespaceHasRustRoutes || r.isSubpathOrParentOfRustRoute);
  }

  // Rust-only routes (registered in Rust without a corresponding Node route file).
  const rustOnlyRoutes = [];
  for (const rp of allRustPaths) {
    if (!matchedRustPaths.has(rp)) {
      const entry = rustByPath.get(rp);
      rustOnlyRoutes.push({
        path: rp,
        methods: [...entry.methods].sort(),
        registrations: entry.registrations,
        handlers: entry.handlers,
        isNonApiAlias: !rp.startsWith("/api/"),
      });
    }
  }

  // Build per-namespace summary.
  const namespaceSummary = {};
  for (const r of nodeRoutes) {
    if (!namespaceSummary[r.namespace]) {
      namespaceSummary[r.namespace] = {
        totalNodeRoutes: 0,
        PARITY_OK: 0,
        PARTIAL_METHODS: 0,
        MISSING: 0,
        nonObviousMissing: 0,
        missingRoutes: [],
        partialRoutes: [],
      };
    }
    const ns = namespaceSummary[r.namespace];
    ns.totalNodeRoutes++;
    ns[r.status]++;
    if (r.status === "PARTIAL_METHODS") {
      ns.partialRoutes.push({
        path: r.path,
        nodeMethods: r.nodeMethods,
        rustMethods: r.rustMethods,
        missingMethods: r.missingMethods,
        reason: r.partialReason,
      });
    } else if (r.status === "MISSING") {
      if (r.isNonObviousMissing) ns.nonObviousMissing++;
      ns.missingRoutes.push({
        path: r.path,
        methods: r.nodeMethods,
        isSubpathOrParentOfRustRoute: r.isSubpathOrParentOfRustRoute,
        rustRelatedAncestors: r.rustRelatedAncestors,
        rustRelatedDescendants: r.rustRelatedDescendants,
      });
    }
  }

  // Method-level counts across all Node route+method pairs.
  let totalRouteMethods = 0;
  let matchedRouteMethods = 0;
  let missingRouteMethods = 0;
  for (const r of nodeRoutes) {
    totalRouteMethods += r.nodeMethods.length;
    missingRouteMethods += r.missingMethods.length;
    matchedRouteMethods += r.nodeMethods.length - r.missingMethods.length;
  }

  const parityOkRoutes = nodeRoutes.filter((r) => r.status === "PARITY_OK");
  const partialMethodsRoutes = nodeRoutes.filter((r) => r.status === "PARTIAL_METHODS");
  const missingRoutes = nodeRoutes.filter((r) => r.status === "MISSING");
  const nonObviousMissingRoutes = missingRoutes.filter((r) => r.isNonObviousMissing);
  const subpathMissingRoutes = missingRoutes.filter((r) => r.isSubpathOrParentOfRustRoute);

  const report = {
    generatedAt: new Date().toISOString(),
    summary: {
      totalNodeRouteFiles: nodeRoutes.length,
      totalNodeRouteMethods: totalRouteMethods,
      matchedNodeRouteMethods: matchedRouteMethods,
      missingNodeRouteMethods: missingRouteMethods,
      counts: {
        PARITY_OK: parityOkRoutes.length,
        PARTIAL_METHODS: partialMethodsRoutes.length,
        MISSING: missingRoutes.length,
      },
      missingBreakdown: {
        nonObviousMissingInPartialNamespaces: nonObviousMissingRoutes.length,
        subpathOrParentOfExistingRustRoute: subpathMissingRoutes.length,
        wholeUnportedNamespaceRoutes: missingRoutes.length - nonObviousMissingRoutes.length,
      },
      totalRustRegisteredPaths: allRustPaths.length,
      totalRustApiPaths: allRustPaths.filter((p) => p.startsWith("/api/")).length,
      totalRustNonApiAliasPaths: allRustPaths.filter((p) => !p.startsWith("/api/")).length,
      rustOnlyApiPaths: rustOnlyRoutes.filter((r) => !r.isNonApiAlias).length,
    },
    focusNamespaces: Object.fromEntries(
      ["providers", "models", "usage", "translator", "settings", "combos", "v1", "v1beta", "provider-nodes", "keys", "admin", "auth", "version"].map(
        (k) => [k, namespaceSummary[k] || null]
      )
    ),
    partialMethodsRoutes: partialMethodsRoutes.map((r) => ({
      path: r.path,
      file: r.file,
      partialReason: r.partialReason,
      nodeMethods: r.nodeMethods,
      rustMethods: r.rustMethods,
      missingMethods: r.missingMethods,
      rustRegistrations: r.rustRegistrations,
    })),
    nonObviousMissingRoutes: nonObviousMissingRoutes.map((r) => ({
      path: r.path,
      file: r.file,
      namespace: r.namespace,
      methods: r.nodeMethods,
      isSubpathOrParentOfRustRoute: r.isSubpathOrParentOfRustRoute,
      rustRelatedAncestors: r.rustRelatedAncestors,
      rustRelatedDescendants: r.rustRelatedDescendants,
      queryParams: r.queryParams,
      bodyFields: r.bodyFields,
    })),
    rustOnlyRoutes,
    namespaceSummary,
    routes: nodeRoutes,
  };

  const outPath = path.join(repoRoot, "scripts", "ast_parity_report.json");
  fs.writeFileSync(outPath, JSON.stringify(report, null, 2) + "\n", "utf8");

  console.log("=== AST Parity Audit Complete ===");
  console.log(`Node route files: ${report.summary.totalNodeRouteFiles}`);
  console.log(
    `Node route+method pairs: ${report.summary.totalNodeRouteMethods} (matched=${matchedRouteMethods}, missing=${missingRouteMethods})`
  );
  console.log(
    `Classification: PARITY_OK=${parityOkRoutes.length}, PARTIAL_METHODS=${partialMethodsRoutes.length}, MISSING=${missingRoutes.length}`
  );
  console.log(
    `Non-obvious MISSING (in partial namespaces / subpaths): ${nonObviousMissingRoutes.length} (direct subpath/parent of Rust route: ${subpathMissingRoutes.length})`
  );
  console.log(`Rust registered paths: ${allRustPaths.length} (Rust-only /api/*: ${report.summary.rustOnlyApiPaths})`);
  console.log(`Report written to: ${outPath}`);
}

main();
