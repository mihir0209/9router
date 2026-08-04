#!/usr/bin/env node
/**
 * Test script to identify minimal required headers for work.freemodel.dev
 * Usage: node scripts/test-workbuddy-headers.mjs <api-key>
 */

import { randomUUID } from "node:crypto";

const API_KEY = process.argv[2];
if (!API_KEY) {
  console.error("Usage: node scripts/test-workbuddy-headers.mjs <api-key>");
  process.exit(1);
}

const BASE_URL = "https://work.freemodel.dev/v1";
const TEST_MODEL = "gpt-5.6-sol";

// Minimal test payload - WorkBuddy uses "agent": "cli" in user messages
const TEST_BODY = {
  model: TEST_MODEL,
  messages: [
    { role: "system", content: "You are a helpful assistant." },
    { role: "user", content: "Reply with exactly: test", agent: "cli" }
  ],
  max_tokens: 10,
  stream: false,
};

// Header combinations to test (ordered from minimal to comprehensive)
const HEADER_SETS = [
  {
    name: "Minimal IDE Identity",
    headers: {
      "X-IDE-Type": "WorkBuddy",
      "X-IDE-Name": "WorkBuddy",
      "x-codebuddy-request": "1",
    },
  },
  {
    name: "IDE + Version",
    headers: {
      "X-IDE-Type": "WorkBuddy",
      "X-IDE-Name": "WorkBuddy",
      "X-IDE-Version": "5.2.7",
      "x-codebuddy-request": "1",
    },
  },
  {
    name: "IDE + User-Agent",
    headers: {
      "X-IDE-Type": "WorkBuddy",
      "X-IDE-Name": "WorkBuddy",
      "X-IDE-Version": "5.2.7",
      "x-codebuddy-request": "1",
      "User-Agent": "WorkBuddy/5.2.7 WorkBuddy/5.2.7 CLI/2.106.4",
    },
  },
  {
    name: "IDE + Agent Context",
    headers: {
      "X-IDE-Type": "WorkBuddy",
      "X-IDE-Name": "WorkBuddy",
      "X-IDE-Version": "5.2.7",
      "x-codebuddy-request": "1",
      "User-Agent": "WorkBuddy/5.2.7 WorkBuddy/5.2.7 CLI/2.106.4",
      "X-Agent-Intent": "craft",
      "X-Agent-Purpose": "conversation",
    },
  },
  {
    name: "IDE + Conversation IDs",
    headers: {
      "X-IDE-Type": "WorkBuddy",
      "X-IDE-Name": "WorkBuddy",
      "X-IDE-Version": "5.2.7",
      "x-codebuddy-request": "1",
      "User-Agent": "WorkBuddy/5.2.7 WorkBuddy/5.2.7 CLI/2.106.4",
      "X-Conversation-ID": randomUUID(),
      "X-Conversation-Request-ID": randomUUID().replace(/-/g, ""),
      "X-Conversation-Message-ID": randomUUID().replace(/-/g, ""),
    },
  },
  {
    name: "IDE + Stainless SDK",
    headers: {
      "X-IDE-Type": "WorkBuddy",
      "X-IDE-Name": "WorkBuddy",
      "X-IDE-Version": "5.2.7",
      "x-codebuddy-request": "1",
      "User-Agent": "WorkBuddy/5.2.7 WorkBuddy/5.2.7 CLI/2.106.4",
      "x-requested-with": "XMLHttpRequest",
      "x-stainless-lang": "js",
      "x-stainless-package-version": "6.25.0",
      "x-stainless-runtime": "node",
      "x-stainless-runtime-version": "v22.21.1",
    },
  },
  {
    name: "Full WorkBuddy Profile",
    headers: {
      "X-IDE-Type": "WorkBuddy",
      "X-IDE-Name": "WorkBuddy",
      "X-IDE-Version": "5.2.7",
      "x-codebuddy-request": "1",
      "User-Agent": "WorkBuddy/5.2.7 WorkBuddy/5.2.7 CLI/2.106.4",
      "X-Agent-Intent": "craft",
      "X-Agent-Purpose": "conversation",
      "X-Conversation-ID": randomUUID(),
      "X-Conversation-Request-ID": randomUUID().replace(/-/g, ""),
      "X-Conversation-Message-ID": randomUUID().replace(/-/g, ""),
      "x-requested-with": "XMLHttpRequest",
      "x-stainless-arch": "x64",
      "x-stainless-lang": "js",
      "x-stainless-os": "Linux",
      "x-stainless-package-version": "6.25.0",
      "x-stainless-retry-count": "0",
      "x-stainless-runtime": "node",
      "x-stainless-runtime-version": "v22.21.1",
      "X-Domain": "www.workbuddy.ai",
      "X-Product": "SaaS",
    },
  },
  {
    name: "Exact Fiddler Capture (with tracing)",
    headers: {
      "x-requested-with": "XMLHttpRequest",
      "x-stainless-arch": "x64",
      "x-stainless-lang": "js",
      "x-stainless-os": "Windows",
      "x-stainless-package-version": "6.25.0",
      "x-stainless-retry-count": "0",
      "x-stainless-runtime": "node",
      "x-stainless-runtime-version": "v22.21.1",
      "X-Conversation-ID": randomUUID(),
      "X-Conversation-Request-ID": randomUUID().replace(/-/g, ""),
      "X-Conversation-Message-ID": randomUUID().replace(/-/g, ""),
      "X-Agent-Intent": "craft",
      "X-Agent-Purpose": "conversation",
      "X-IDE-Type": "WorkBuddy",
      "X-IDE-Name": "WorkBuddy",
      "X-IDE-Version": "5.2.7",
      "X-Request-ID": randomUUID().replace(/-/g, ""),
      "acp-connection-id": randomUUID(),
      "x-codebuddy-request": "1",
      "traceparent": () => {
        const traceId = randomUUID().replace(/-/g, "");
        const spanId = randomUUID().replace(/-/g, "").substring(0, 16);
        return `00-${traceId}-${spanId}-01`;
      },
      "b3": () => {
        const traceId = randomUUID().replace(/-/g, "");
        const spanId = randomUUID().replace(/-/g, "").substring(0, 16);
        const parentSpanId = randomUUID().replace(/-/g, "").substring(0, 16);
        return `${traceId}-${spanId}-1-${parentSpanId}`;
      },
      "X-B3-TraceId": () => randomUUID().replace(/-/g, ""),
      "X-B3-ParentSpanId": () => randomUUID().replace(/-/g, "").substring(0, 16),
      "X-B3-SpanId": () => randomUUID().replace(/-/g, "").substring(0, 16),
      "X-B3-Sampled": "1",
      "X-Trace-ID": () => randomUUID().replace(/-/g, ""),
      "X-User-Id": randomUUID(),
      "X-Domain": "www.workbuddy.ai",
      "X-Product": "SaaS",
      "User-Agent": "WorkBuddy/5.2.7 WorkBuddy/5.2.7 CLI/2.106.4",
    },
  },
];

async function testHeaderSet(name, customHeaders) {
  // Resolve function-based header values
  const resolvedHeaders = {};
  for (const [key, value] of Object.entries(customHeaders)) {
    resolvedHeaders[key] = typeof value === "function" ? value() : value;
  }

  const headers = {
    "Content-Type": "application/json",
    Accept: "application/json",
    "X-API-Key": API_KEY,
    Authorization: `Bearer ${API_KEY}`,
    ...resolvedHeaders,
  };

  try {
    const response = await fetch(`${BASE_URL}/chat/completions`, {
      method: "POST",
      headers,
      body: JSON.stringify(TEST_BODY),
    });

    const text = await response.text();
    let data;
    try {
      data = JSON.parse(text);
    } catch {
      data = { _rawText: text };
    }

    if (response.ok) {
      return {
        success: true,
        status: response.status,
        hasContent: !!data.choices?.[0]?.message?.content,
        preview: data.choices?.[0]?.message?.content?.substring(0, 50),
      };
    } else {
      return {
        success: false,
        status: response.status,
        error: data.error?.message || data.error || text.substring(0, 200),
        code: data.error?.code,
      };
    }
  } catch (err) {
    return {
      success: false,
      error: err.message,
      networkError: true,
    };
  }
}

async function main() {
  console.log("Testing work.freemodel.dev header requirements\n");
  console.log("=".repeat(80));

  const results = [];

  for (const set of HEADER_SETS) {
    process.stdout.write(`\n[${set.name}] Testing... `);
    const result = await testHeaderSet(set.name, set.headers);
    results.push({ name: set.name, ...result });

    if (result.success) {
      console.log("✓ SUCCESS");
      console.log(`  Status: ${result.status}`);
      if (result.hasContent) {
        console.log(`  Response: "${result.preview}..."`);
      }
    } else {
      console.log("✗ FAILED");
      console.log(`  Status: ${result.status || "N/A"}`);
      console.log(`  Error: ${result.error}`);
      if (result.code) {
        console.log(`  Code: ${result.code}`);
      }
    }
  }

  console.log("\n" + "=".repeat(80));
  console.log("\nSummary:");
  console.log("-".repeat(80));

  const firstSuccess = results.find((r) => r.success);
  if (firstSuccess) {
    console.log(`\n✓ Minimal working set: "${firstSuccess.name}"\n`);
    const successfulSet = HEADER_SETS.find((s) => s.name === firstSuccess.name);
    console.log("Required headers:");
    for (const [key, value] of Object.entries(successfulSet.headers)) {
      // Show template for dynamic values
      const displayValue = key.includes("Conversation") || key.includes("Request-ID")
        ? "<random-uuid>"
        : value;
      console.log(`  ${key}: ${displayValue}`);
    }
  } else {
    console.log("\n✗ No header combination succeeded");
    console.log("\nAll attempts failed. Possible reasons:");
    console.log("  - API key invalid or lacks access");
    console.log("  - Additional authentication mechanism required");
    console.log("  - IP-based restrictions");
    console.log("  - Session/token establishment needed");
  }

  console.log("\n" + "=".repeat(80));
}

main().catch(console.error);
