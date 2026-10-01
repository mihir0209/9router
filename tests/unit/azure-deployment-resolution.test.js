import { describe, it, expect, beforeEach, afterEach } from "vitest";

import {
  AzureExecutor,
  normalizeAzureModelPool,
  resolveAzureDeployment,
  azureDeploymentMatches
} from "open-sse/executors/azure.js";
import { scopeConnectionsByDeployment } from "@/sse/services/azureDeploymentScope.js";

// Azure routes on the URL path — `/deployments/{d}/chat/completions` — so the
// `model` in the request body is ignored upstream and the deployment alone decides
// which model answers. The stored `deployment` used to be first in an `||` chain,
// which made it unconditionally win while the configured pool stayed display-only.
//
// Two properties are pinned here. The deployment must still win by default,
// because the single-deployment connections that motivated the original design
// rely on it and have no pool configured. And a model the admin explicitly listed
// in a connection's pool must be routable, otherwise the pool is a lie.

// `psd` is a connection's providerSpecificData blob — the shape
// resolveAzureDeployment reads. `conn` is a whole connection — the shape
// azureDeploymentMatches reads. Keeping these distinct matters: passing a
// connection where psd is expected hides `deployment` entirely.
const psd = (deployment, pool, key = "enabledModels") => ({ deployment, [key]: pool });
const conn = (deployment, pool, key = "enabledModels") => ({ providerSpecificData: psd(deployment, pool, key) });

describe("normalizeAzureModelPool", () => {
  it("returns an empty pool when nothing is configured", () => {
    expect(normalizeAzureModelPool(undefined)).toEqual([]);
    expect(normalizeAzureModelPool({})).toEqual([]);
    expect(normalizeAzureModelPool({ models: "gpt-4" })).toEqual([]);
  });

  it("strips the provider prefix and trims", () => {
    expect(normalizeAzureModelPool({ models: ["azure/gpt-5.6-terra", " gpt-4 "] }))
      .toEqual(["gpt-5.6-terra", "gpt-4"]);
  });

  it("reads enabledModels when models is absent", () => {
    expect(normalizeAzureModelPool({ enabledModels: ["gpt-4"] })).toEqual(["gpt-4"]);
  });
});

describe("resolveAzureDeployment", () => {
  const env = { before: process.env.AZURE_DEPLOYMENT };
  beforeEach(() => { delete process.env.AZURE_DEPLOYMENT; });
  afterEach(() => {
    if (env.before === undefined) delete process.env.AZURE_DEPLOYMENT;
    else process.env.AZURE_DEPLOYMENT = env.before;
  });

  it("keeps the stored deployment when no pool is configured (back-compat)", () => {
    expect(resolveAzureDeployment({ deployment: "gpt-5.6-terra" }, "anything-else"))
      .toBe("gpt-5.6-terra");
  });

  it("uses the requested model when the pool lists it", () => {
    expect(resolveAzureDeployment(psd("gpt-5.6-terra", ["gpt-5.6-sol"]), "gpt-5.6-sol"))
      .toBe("gpt-5.6-sol");
  });

  it("falls back to the deployment when the pool does not list the model", () => {
    expect(resolveAzureDeployment(psd("gpt-5.6-terra", ["gpt-5.6-sol"]), "gpt-5.6-sol2"))
      .toBe("gpt-5.6-terra");
  });

  it("uses the requested model when the connection has no deployment", () => {
    expect(resolveAzureDeployment({ models: [] }, "gpt-4o")).toBe("gpt-4o");
    expect(resolveAzureDeployment({}, "gpt-4o")).toBe("gpt-4o");
  });

  it("strips the azure/ prefix from the requested model", () => {
    expect(resolveAzureDeployment(psd("gpt-5.6-terra", ["azure/gpt-5.6-sol"]), "azure/gpt-5.6-sol"))
      .toBe("gpt-5.6-sol");
  });

  it("ignores an empty pool", () => {
    expect(resolveAzureDeployment({ deployment: "d", enabledModels: [] }, "m")).toBe("d");
  });

  it("falls through to AZURE_DEPLOYMENT then gpt-4", () => {
    process.env.AZURE_DEPLOYMENT = "env-deploy";
    expect(resolveAzureDeployment({}, null)).toBe("env-deploy");
    delete process.env.AZURE_DEPLOYMENT;
    expect(resolveAzureDeployment({}, null)).toBe("gpt-4");
  });
});

describe("azureDeploymentMatches", () => {
  it("matches when the pool lists the model", () => {
    expect(azureDeploymentMatches(conn("gpt-5.6-terra", ["gpt-5.6-sol"]), "gpt-5.6-sol")).toBe(true);
  });

  it("matches when the deployment equals the requested model", () => {
    expect(azureDeploymentMatches(conn("gpt-5.6-terra"), "gpt-5.6-terra")).toBe(true);
    expect(azureDeploymentMatches(conn("FW-Kimi-K3"), "fw-kimi-k3")).toBe(true);
  });

  it("does not match a different pinned deployment", () => {
    expect(azureDeploymentMatches(conn("gpt-5.6-sol"), "gpt-5.6-terra")).toBe(false);
  });

  it("never matches an unpinned connection", () => {
    expect(azureDeploymentMatches({ providerSpecificData: {} }, "gpt-4o")).toBe(false);
    expect(azureDeploymentMatches(null, "gpt-4o")).toBe(false);
  });
});

describe("scopeConnectionsByDeployment", () => {
  const pool = conn("gpt-5.6-terra", ["gpt-5.6-terra"]);
  const sol = conn("gpt-5.6-sol", ["gpt-5.6-sol"]);
  const kimi = conn("FW-Kimi-K3", ["fw-kimi-k3"]);

  it("narrows to the connection that can serve the model", () => {
    const scoped = scopeConnectionsByDeployment([pool, sol, kimi], "azure", "azure/gpt-5.6-sol");
    expect(scoped).toEqual([sol]);
  });

  it("returns the full set when nothing matches, preserving fallback", () => {
    const all = [pool, sol];
    expect(scopeConnectionsByDeployment(all, "azure", "azure/unknown-model")).toBe(all);
  });

  it("returns the full set when every connection matches", () => {
    const all = [pool, sol];
    expect(scopeConnectionsByDeployment(all, "azure", "azure/gpt-5.6-terra")).toEqual([pool]);
  });

  it("leaves non-azure providers untouched", () => {
    const all = [pool, sol];
    expect(scopeConnectionsByDeployment(all, "openai", "gpt-5.6-sol")).toBe(all);
  });

  it("leaves a single connection untouched", () => {
    const one = [pool];
    expect(scopeConnectionsByDeployment(one, "azure", "azure/other")).toBe(one);
  });
});

describe("AzureExecutor.buildUrl", () => {
  const executor = new AzureExecutor();

  it("builds the pinned deployment URL", () => {
    const url = executor.buildUrl("gpt-5.6-terra", false, 0, {
      providerSpecificData: {
        azureEndpoint: "https://x.openai.azure.com",
        apiVersion: "2024-10-01-preview",
        deployment: "gpt-5.6-sol"
      }
    });
    expect(url).toBe("https://x.openai.azure.com/openai/deployments/gpt-5.6-sol/chat/completions?api-version=2024-10-01-preview");
  });

  it("honours a pooled model over the pinned deployment", () => {
    const url = executor.buildUrl("gpt-5.6-sol", false, 0, {
      providerSpecificData: {
        azureEndpoint: "https://x.openai.azure.com",
        apiVersion: "v",
        deployment: "gpt-5.6-terra",
        models: ["gpt-5.6-sol"]
      }
    });
    expect(url).toContain("/deployments/gpt-5.6-sol/");
  });

  it("trims a trailing slash on the endpoint", () => {
    const url = executor.buildUrl("m", false, 0, {
      providerSpecificData: { azureEndpoint: "https://x.openai.azure.com/", apiVersion: "v", deployment: "d" }
    });
    expect(url.startsWith("https://x.openai.azure.com/openai/")).toBe(true);
  });
});