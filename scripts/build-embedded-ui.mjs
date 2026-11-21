import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const projectRoot = path.resolve(__dirname, "..");
const nextDir = path.join(projectRoot, ".next");
const publicDir = path.join(projectRoot, "public");
const targetDir = path.join(projectRoot, "rust", "app", "ui");

console.log("====================================================");
console.log("⚡ Building Authentic 9Router Next.js Embedded UI");
console.log("====================================================");

if (!fs.existsSync(nextDir)) {
  console.error("❌ Error: .next directory not found. Please run 'npm run build' first.");
  process.exit(1);
}

// 1. Clean and prepare target directory
if (fs.existsSync(targetDir)) {
  fs.rmSync(targetDir, { recursive: true, force: true });
}
fs.mkdirSync(targetDir, { recursive: true });

// 2. Copy public directory assets (favicons, logos, manifests, provider images)
if (fs.existsSync(publicDir)) {
  fs.cpSync(publicDir, targetDir, { recursive: true });
  console.log("✓ Copied public assets to embedded UI");
}

// 3. Copy Next.js static assets (_next/static)
const staticSource = path.join(nextDir, "static");
const staticDest = path.join(targetDir, "_next", "static");
if (fs.existsSync(staticSource)) {
  fs.mkdirSync(path.dirname(staticDest), { recursive: true });
  fs.cpSync(staticSource, staticDest, { recursive: true });
  console.log("✓ Copied .next/static to _next/static");
} else {
  console.warn("⚠️ Warning: .next/static not found");
}

// 4. Recursively copy all prerendered HTML and RSC files from .next/server/app
const serverAppDir = path.join(nextDir, "server", "app");
let htmlCount = 0;
let rscCount = 0;

function copyHtmlFiles(dir, relativeBase = "") {
  if (!fs.existsSync(dir)) return;
  const entries = fs.readdirSync(dir, { withFileTypes: true });

  for (const entry of entries) {
    const fullPath = path.join(dir, entry.name);
    const relPath = path.join(relativeBase, entry.name);

    if (entry.isDirectory()) {
      // Skip internal api routes and segment folders
      if (entry.name !== "api" && !entry.name.endsWith(".segments")) {
        copyHtmlFiles(fullPath, relPath);
      }
    } else if (entry.name.endsWith(".html") || entry.name.endsWith(".rsc")) {
      const destPath = path.join(targetDir, relPath);
      fs.mkdirSync(path.dirname(destPath), { recursive: true });
      fs.copyFileSync(fullPath, destPath);
      if (entry.name.endsWith(".html")) htmlCount++;
      else rscCount++;
    }
  }
}

if (fs.existsSync(serverAppDir)) {
  copyHtmlFiles(serverAppDir);
  console.log(`✓ Copied ${htmlCount} HTML pages and ${rscCount} RSC payloads from .next/server/app`);
}

// 5. Ensure root fallback: if index.html doesn't exist, use dashboard.html
const indexHtml = path.join(targetDir, "index.html");
const dashboardHtml = path.join(targetDir, "dashboard.html");
if (!fs.existsSync(indexHtml) && fs.existsSync(dashboardHtml)) {
  fs.copyFileSync(dashboardHtml, indexHtml);
  console.log("✓ Created index.html fallback from dashboard.html");
}

// 6. Mirror bracketed chunk directories/files with percent-encoded names (%5B and %5D)
function mirrorUrlEncodedPaths(dir) {
  if (!fs.existsSync(dir)) return;
  const entries = fs.readdirSync(dir, { withFileTypes: true });
  for (const entry of entries) {
    const fullPath = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      mirrorUrlEncodedPaths(fullPath);
      if (entry.name.includes("[") || entry.name.includes("]")) {
        const encodedName = entry.name.replaceAll("[", "%5B").replaceAll("]", "%5D");
        const encodedPath = path.join(dir, encodedName);
        if (!fs.existsSync(encodedPath)) {
          fs.cpSync(fullPath, encodedPath, { recursive: true });
        }
      }
    } else if (entry.isFile() && (entry.name.includes("[") || entry.name.includes("]"))) {
      const encodedName = entry.name.replaceAll("[", "%5B").replaceAll("]", "%5D");
      const encodedPath = path.join(dir, encodedName);
      if (!fs.existsSync(encodedPath)) {
        fs.copyFileSync(fullPath, encodedPath);
      }
    }
  }
}
mirrorUrlEncodedPaths(targetDir);
console.log("✓ Mirrored percent-encoded paths for bracketed routes (%5B...%5D)");

console.log("====================================================");
console.log(`🎉 Authentic 9Router embedded UI ready at ${targetDir}`);
console.log("====================================================");
