import { NextResponse } from "next/server";
import { access, constants } from "fs/promises";
import { homedir } from "os";
import { join } from "path";
import { execFile } from "child_process";
import { promisify } from "util";

const execFileAsync = promisify(execFile);

const SUPPORTED_PLATFORMS = new Set(["darwin", "win32", "linux"]);
const PLATFORM_LABELS = { darwin: "macOS", win32: "Windows" };

const ACCESS_TOKEN_KEYS = ["cursorAuth/accessToken", "cursorAuth/token"];
const MACHINE_ID_KEYS = [
  "storage.serviceMachineId",
  "storage.machineId",
  "telemetry.machineId",
];

const FUZZY_TOKEN_NEEDLES = ["accesstoken", "access_token"];
const FUZZY_MACHINE_NEEDLES = ["machineid", "machine_id"];

/** Candidate db paths, probed in priority order. */
function getCandidatePaths(platform) {
  const home = homedir();

  if (platform === "darwin") {
    return [
      join(
        home,
        "Library/Application Support/Cursor/User/globalStorage/state.vscdb",
      ),
      join(
        home,
        "Library/Application Support/Cursor - Insiders/User/globalStorage/state.vscdb",
      ),
    ];
  }

  if (platform === "win32") {
    const appData = process.env.APPDATA || join(home, "AppData", "Roaming");
    const localAppData =
      process.env.LOCALAPPDATA || join(home, "AppData", "Local");
    return [
      join(appData, "Cursor", "User", "globalStorage", "state.vscdb"),
      join(
        appData,
        "Cursor - Insiders",
        "User",
        "globalStorage",
        "state.vscdb",
      ),
      join(localAppData, "Cursor", "User", "globalStorage", "state.vscdb"),
      join(
        localAppData,
        "Programs",
        "Cursor",
        "User",
        "globalStorage",
        "state.vscdb",
      ),
    ];
  }

  return [join(home, ".config/Cursor/User/globalStorage/state.vscdb")];
}

/** Cursor stores some values JSON-encoded; unwrap to the raw string. */
const normalize = (value) => {
  if (typeof value !== "string") return value;
  try {
    const parsed = JSON.parse(value);
    return typeof parsed === "string" ? parsed : value;
  } catch {
    return value;
  }
};

const inClause = (keys) => keys.map(() => "?").join(", ");

const selectByKeys = (db, keys) => {
  const rows = db
    .prepare(`SELECT key, value FROM itemTable WHERE key IN (${inClause(keys)})`)
    .all();
  for (const key of keys) {
    const row = rows.find((r) => r?.key === key);
    if (row?.value) return normalize(row.value);
  }
  return null;
};

const selectFuzzy = (db, needles) => {
  const rows = db
    .prepare(
      `SELECT key, value FROM itemTable WHERE ${needles
        .map((n) => `key LIKE '%${n}%'`)
        .join(" OR ")}`,
    )
    .all();
  for (const needle of needles) {
    const row = rows.find(
      (r) => typeof r?.key === "string" && r.key.toLowerCase().includes(needle),
    );
    if (row?.value) return normalize(row.value);
  }
  return null;
};

/**
 * Read tokens from an open database. Exact keys first, then a fuzzy match so
 * renamed/relocated keys in newer Cursor builds still resolve.
 */
function extractFromDb(db, { allowFuzzy }) {
  let accessToken = selectByKeys(db, ACCESS_TOKEN_KEYS);
  let machineId = selectByKeys(db, MACHINE_ID_KEYS);

  if (allowFuzzy && (!accessToken || !machineId)) {
    accessToken ||= selectFuzzy(db, FUZZY_TOKEN_NEEDLES);
    machineId ||= selectFuzzy(db, FUZZY_MACHINE_NEEDLES);
  }

  return { accessToken, machineId };
}

const found = (tokens) => ({ found: true, ...tokens });

const notFound = (error, extra = {}) => ({ found: false, error, ...extra });

/**
 * better-sqlite3 is an optional native dependency. Returns null when its
 * bindings are unavailable so the caller can fall back to the sqlite3 CLI.
 */
async function openWithBetterSqlite(dbPath) {
  try {
    const mod = await import("better-sqlite3");
    const Database = mod.default ?? mod;
    return new Database(dbPath, { readonly: true, fileMustExist: true });
  } catch (error) {
    if (
      error?.code === "ERR_MODULE_NOT_FOUND" ||
      /Cannot find module|bindings|NODE_MODULE_VERSION/i.test(
        error?.message ?? "",
      )
    ) {
      return null;
    }
    throw error;
  }
}

/** Fallback when the native bindings are missing; needs the sqlite3 binary. */
async function extractTokensViaCLI(dbPath) {
  const readAll = async (sql) => {
    const { stdout } = await execFileAsync("sqlite3", [dbPath, sql], {
      timeout: 10000,
    });
    return stdout
      .split("\n")
      .filter(Boolean)
      .map((line) => {
        const [key, ...rest] = line.split("|");
        return { key, value: rest.join("|") };
      });
  };

  const byKeys = async (keys) => {
    const sql = `SELECT key, value FROM itemTable WHERE key IN (${keys
      .map((k) => `'${k}'`)
      .join(",")})`;
    const rows = await readAll(sql);
    for (const key of keys) {
      const row = rows.find((r) => r?.key === key);
      if (row?.value) return normalize(row.value);
    }
    return null;
  };

  return {
    accessToken: await byKeys(ACCESS_TOKEN_KEYS),
    machineId: await byKeys(MACHINE_ID_KEYS),
  };
}

/**
 * GET /api/oauth/cursor/auto-import
 * Auto-detect and extract Cursor tokens from the local Cursor SQLite database.
 */
export async function GET() {
  const platform = process.platform;

  if (!SUPPORTED_PLATFORMS.has(platform)) {
    return NextResponse.json(
      { found: false, error: "Unsupported platform" },
      { status: 400 },
    );
  }

  try {
    // Linux has exactly one known path, so probe straight to it rather than
    // stat-ing candidates we would only ignore anyway.
    const candidates = getCandidatePaths(platform);
    const singleKnownPath = candidates.length === 1;

    let dbPath = null;
    if (singleKnownPath) {
      dbPath = candidates[0];
    } else {
      for (const candidate of candidates) {
        try {
          await access(candidate, constants.R_OK);
          dbPath = candidate;
          break;
        } catch {
          // Try next candidate
        }
      }

      if (!dbPath) {
        return NextResponse.json(
          notFound(
            `Cursor database not found in known ${PLATFORM_LABELS[platform]} locations:\n${candidates.join("\n")}\n\nMake sure Cursor IDE is installed and opened at least once.`,
          ),
        );
      }
    }

    let db = null;
    try {
      db = await openWithBetterSqlite(dbPath);
    } catch (error) {
      // Only probed platforms confirmed the file exists, so only they can
      // honestly report "found it but could not open it" instead of falling
      // through to a generic "paste manually" that hides the real cause.
      if (!singleKnownPath) {
        return NextResponse.json(
          notFound(
            `Found a Cursor database at ${dbPath} but could not open it: ${error?.message ?? error}`,
            { dbPath },
          ),
        );
      }
    }

    if (db) {
      let tokens;
      try {
        tokens = extractFromDb(db, { allowFuzzy: platform === "darwin" });
      } finally {
        db.close();
      }
      if (tokens.accessToken && tokens.machineId) {
        return NextResponse.json(found(tokens));
      }
    } else {
      try {
        const tokens = await extractTokensViaCLI(dbPath);
        if (tokens.accessToken && tokens.machineId) {
          return NextResponse.json(found(tokens));
        }
      } catch {
        // sqlite3 CLI unavailable too
      }
    }

    if (singleKnownPath) {
      return NextResponse.json(
        notFound(
          "Cursor database not found. Make sure Cursor IDE is installed and you are logged in.",
        ),
      );
    }

    return NextResponse.json(
      notFound(
        "Could not find Cursor tokens in the database. Please login to Cursor IDE first, then retry.",
        { windowsManual: true, dbPath },
      ),
    );
  } catch (error) {
    console.log("Cursor auto-import error:", error);
    return NextResponse.json(
      { found: false, error: error.message },
      { status: 500 },
    );
  }
}
