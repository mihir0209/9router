# Custom Headers for OpenAI/Anthropic-Compatible Providers

Custom header injection allows 9Router to impersonate specific clients when forwarding requests to OpenAI-compatible and Anthropic-compatible provider nodes. This is useful for providers that validate client identity through HTTP headers.

## Overview

The custom header system provides:

1. **Header Injection** - Add custom headers with static or dynamic values before authentication
2. **Template Variables** - Generate unique IDs, timestamps, and random values per request
3. **Header Blacklist** - Remove specific headers after all headers are set (case-insensitive)

## UI Location

For OpenAI-compatible and Anthropic-compatible provider nodes:

**Dashboard → Providers → [Your Compatible Node] → Custom Headers**

## Template Variables

Use placeholders in header values to generate dynamic content per request:

| Template | Output | Example |
|----------|--------|---------|
| `{uuid}` | UUID with dashes | `f365a7e7-0831-4306-9784-df0cd82fd320` |
| `{uuid-nodash}` | UUID without dashes | `f365a7e708314306978df0cd82fd320` |
| `{timestamp}` | Unix timestamp (ms) | `1722792000000` |
| `{timestamp-sec}` | Unix timestamp (sec) | `1722792000` |
| `{random-hex-8}` | 8 hex characters | `a3f7c2b1` |
| `{random-hex-16}` | 16 hex characters | `a3f7c2b1d4e56890` |
| `{random-hex-32}` | 32 hex characters | `a3f7c2b1d4e56890f1a2b3c4d5e6f7a8` |
| `{iso8601}` | ISO 8601 timestamp | `2026-08-04T19:28:00.000Z` |

Templates can be combined in one header value:

```
traceparent: 00-{uuid-nodash}-{random-hex-16}-01
X-Request-ID: {uuid}
User-Agent: MyClient/1.0.0 Build/{timestamp}
```

## Example: WorkBuddy Client Identity

To impersonate WorkBuddy client (note: this example won't work with `work.freemodel.dev` due to TLS fingerprinting, but will work with providers that only validate headers):

**Custom Headers:**
```
X-IDE-Type: WorkBuddy
X-IDE-Name: WorkBuddy
X-IDE-Version: 5.2.7
x-codebuddy-request: 1
X-Conversation-ID: {uuid}
X-Conversation-Request-ID: {uuid-nodash}
X-Request-ID: {uuid-nodash}
traceparent: 00-{uuid-nodash}-{random-hex-16}-01
User-Agent: WorkBuddy/5.2.7 CLI/2.106.4
```

**Header Blacklist:**
```
user-agent, x-stainless-lang, x-stainless-runtime
```

## How It Works

### Request Flow

1. **Custom Headers Applied First** - Before authentication, templates are resolved and headers are injected
2. **Authentication Headers Added** - `Authorization`, `x-api-key`, `anthropic-version` etc.
3. **Claude Identity Filtering** (if enabled) - Removes Claude Code headers for non-Anthropic upstreams
4. **Blacklist Applied Last** - Removes specified headers (case-insensitive matching)

### Storage

Custom headers and blacklist are stored in the provider node configuration:

```json
{
  "id": "openai-compatible-chat-...",
  "type": "openai-compatible",
  "customHeaders": {
    "X-IDE-Type": "WorkBuddy",
    "X-Request-ID": "{uuid-nodash}"
  },
  "headerBlacklist": ["user-agent", "x-stainless-lang"]
}
```

When a connection is loaded, these fields are merged into `credentials.providerSpecificData` and consumed by `BaseExecutor.buildHeaders()` and `DefaultExecutor.buildHeaders()`.

## Limitations

### TLS Fingerprinting

Some providers (e.g., `work.freemodel.dev`) use **TLS fingerprinting** to detect the client's TLS stack. Node.js and Electron/Chromium have different TLS fingerprints. Header injection alone cannot bypass TLS fingerprinting.

**Symptoms:**
- All header combinations fail with client validation errors
- Provider returns different errors for different clients (e.g., "Claude Code not supported" vs "WorkBuddy only")
- First request fails, retry succeeds (session establishment)

**Workaround:**
- Use the provider's official endpoint that accepts your client identity
- Contact the provider to allow 9Router's client signature
- Use a TLS termination proxy that matches the expected fingerprint

### Request Body Identity

Some providers validate identity through request body structure (e.g., custom metadata fields, specific tool schemas). Custom headers do not modify the request body.

## Testing

Test script to verify minimal required headers:

```bash
node scripts/test-workbuddy-headers.mjs <api-key>
```

The script tests combinations from minimal to comprehensive and reports which headers are required.

## Code Reference

- **Template Resolution**: `open-sse/utils/headerTemplates.js`
- **Header Injection**: `open-sse/executors/base.js` (line 56), `open-sse/executors/default.js` (line 180)
- **Provider Node Storage**: `src/lib/db/repos/nodesRepo.js`
- **Credentials Enrichment**: `src/sse/services/auth.js` (line 181)
- **UI Modal**: `src/app/(dashboard)/dashboard/providers/[id]/CustomHeadersModal.js`
- **Tests**: `tests/unit/header-templates.test.js`

## Security Notes

- Custom headers are **not sanitized** - avoid injecting untrusted user input
- Templates generate **cryptographically secure random values** via Node.js `crypto` module
- Header blacklist matching is **case-insensitive** to prevent bypass via casing
- Authorization headers (`Authorization`, `x-api-key`, `cookie`) are **never logged** in observability records
