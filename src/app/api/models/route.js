import { NextResponse } from "next/server";
import { getModelAliases, setModelAlias, getCustomModels } from "@/models";
import { getDisabledModels } from "@/lib/disabledModelsDb";
import { AI_MODELS } from "@/shared/constants/config";
import { getProviderAlias } from "@/shared/constants/providers";
import { getCapabilitiesForModel } from "open-sse/providers/capabilities.js";

// GET /api/models - Get models with aliases
export async function GET() {
  try {
    const [modelAliases, disabled] = await Promise.all([
      getModelAliases(),
      getDisabledModels(),
    ]);

      // full model id -> the alias that points at it, built once.
      //
      // This used to be a linear scan of every alias, inside the per-model map:
      // Object.entries(modelAliases).find(([, v]) => v === fullModel)
      // That is models x aliases comparisons on every request — with a few
      // thousand catalogue models it is millions of string compares, and the
      // dashboard fetches this route whenever it renders a model list, which is
      // what pushed a combo save into the Workers CPU ceiling (error 1102).
      const aliasByFullModel = new Map();
      for (const [aliasKey, target] of Object.entries(modelAliases || {})) {
        if (!aliasByFullModel.has(target)) aliasByFullModel.set(target, aliasKey);
      }

      const models = AI_MODELS
        .filter((m) => {
          const alias = getProviderAlias(m.provider) || m.provider;
          const list = disabled[alias] || disabled[m.provider] || [];
          return !list.includes(m.model);
        })
        .map((m) => {
          const fullModel = `${m.provider}/${m.model}`;
          const providerAlias = getProviderAlias(m.provider) || m.provider;
          const routedModel = `${providerAlias}/${m.model}`;
          const c = getCapabilitiesForModel(m.provider, m.model);
          const aliasForModel = aliasByFullModel.get(fullModel);
          return {
          ...m,
          fullModel,
          routedModel,
          alias: aliasForModel || m.model,
          caps: {
            vision: c.vision,
            search: c.search,
            reasoning: c.reasoning,
            contextWindow: c.contextWindow,
            maxOutput: c.maxOutput,
          },
        };
      });

    // Custom models ride along; their stored caps override the name heuristic
    const seenFull = new Set(models.map((m) => m.fullModel));
    const customModels = (await getCustomModels()).filter((m) => {
      if (!m?.id || (m.kind || m.type || "llm") !== "llm") return false;
      return !seenFull.has(`${m.providerAlias}/${m.id}`);
    });
    for (const m of customModels) {
      const fullModel = `${m.providerAlias}/${m.id}`;
      const c = getCapabilitiesForModel(m.providerAlias, m.id);
      models.push({
        provider: m.providerAlias,
        model: m.id,
        name: m.name || m.id,
        fullModel,
        routedModel: fullModel,
        alias: modelAliases[fullModel] || m.id,
        caps: {
          vision: c.vision,
          search: c.search,
          reasoning: c.reasoning,
          contextWindow: c.contextWindow,
          maxOutput: c.maxOutput,
          ...(m.caps || {}),
        },
      });
    }

    return NextResponse.json({ models });
  } catch (error) {
    console.log("Error fetching models:", error);
    return NextResponse.json({ error: "Failed to fetch models" }, { status: 500 });
  }
}

// PUT /api/models - Update model alias
export async function PUT(request) {
  try {
    const body = await request.json();
    const { model, alias } = body;

    if (!model || !alias) {
      return NextResponse.json({ error: "Model and alias required" }, { status: 400 });
    }

    const modelAliases = await getModelAliases();

    // Check if alias already exists for different model
    const existingModel = Object.entries(modelAliases).find(
      ([key, val]) => val === alias && key !== model
    );

    if (existingModel) {
      return NextResponse.json({ error: "Alias already in use" }, { status: 400 });
    }

    // Update alias
    await setModelAlias(alias, model);

    return NextResponse.json({ success: true, model, alias });
  } catch (error) {
    console.log("Error updating alias:", error);
    return NextResponse.json({ error: "Failed to update alias" }, { status: 500 });
  }
}
