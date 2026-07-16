"use client";

import { useState } from "react";
import PropTypes from "prop-types";
import { Button } from "@/shared/components";
import { getProviderCustomModelRows } from "@/shared/utils/providerCustomModels";

function CompatibleModelRow({ modelId, fullModel, copied, onCopy, onDeleteAlias, onTest, testStatus, isTesting, displayName, onSetDisplayName, aliasId, onSetAliasId }) {
  const [editingDisplayName, setEditingDisplayName] = useState(false);
  const [displayNameValue, setDisplayNameValue] = useState(displayName || "");
  const [editingAliasId, setEditingAliasId] = useState(false);
  const [aliasIdValue, setAliasIdValue] = useState(aliasId || "");

  const borderColor = testStatus === "ok"
    ? "border-green-500/40"
    : testStatus === "error"
    ? "border-red-500/40"
    : "border-border";

  const iconColor = testStatus === "ok"
    ? "#22c55e"
    : testStatus === "error"
    ? "#ef4444"
    : undefined;

  const showAs = displayName || aliasId || modelId;

  const handleSaveDisplayName = () => {
    if (displayNameValue.trim() && onSetDisplayName) {
      onSetDisplayName(fullModel, displayNameValue.trim());
    }
    setEditingDisplayName(false);
  };

  const handleCancelDisplayName = () => {
    setDisplayNameValue(displayName || "");
    setEditingDisplayName(false);
  };

  const handleSaveAliasId = () => {
    if (aliasIdValue.trim() && onSetAliasId) {
      onSetAliasId(fullModel, aliasIdValue.trim());
    }
    setEditingAliasId(false);
  };

  const handleCancelAliasId = () => {
    setAliasIdValue(aliasId || "");
    setEditingAliasId(false);
  };

  return (
    <div className={`flex items-center gap-3 p-3 rounded-lg border ${borderColor} hover:bg-sidebar/50`}>
      <span
        className="material-symbols-outlined text-base text-text-muted"
        style={iconColor ? { color: iconColor } : undefined}
      >
        {testStatus === "ok" ? "check_circle" : testStatus === "error" ? "cancel" : "smart_toy"}
      </span>
      <div className="flex-1 min-w-0">
        {/* Display name editing + alias editing inline */}
        <div className="flex flex-wrap items-center gap-1">
          {/* Display Name editing */}
          {editingDisplayName ? (
            <div className="flex items-center gap-0.5">
              <input
                type="text"
                value={displayNameValue}
                onChange={(e) => setDisplayNameValue(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") handleSaveDisplayName();
                  if (e.key === "Escape") handleCancelDisplayName();
                }}
                className="flex-1 min-w-0 rounded bg-sidebar px-1.5 py-0.5 font-mono text-xs text-text-main border border-primary/30 focus:outline-none focus:ring-1 focus:ring-primary/50"
                placeholder="Display name (e.g. DeepSeek v4)"
                autoFocus
              />
              <button
                onClick={handleSaveDisplayName}
                className="rounded p-0.5 text-green-500 hover:bg-green-500/10"
                title="Save display name"
              >
                <span className="material-symbols-outlined text-sm">check</span>
              </button>
              <button
                onClick={handleCancelDisplayName}
                className="rounded p-0.5 text-text-muted hover:bg-sidebar"
                title="Cancel"
              >
                <span className="material-symbols-outlined text-sm">close</span>
              </button>
            </div>
          ) : null}

          {/* Alias editing */}
          {editingAliasId ? (
            <div className="flex items-center gap-0.5">
              <input
                type="text"
                value={aliasIdValue}
                onChange={(e) => setAliasIdValue(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") handleSaveAliasId();
                  if (e.key === "Escape") handleCancelAliasId();
                }}
                className="flex-1 min-w-0 rounded bg-sidebar px-1.5 py-0.5 font-mono text-xs text-text-main border border-primary/30 focus:outline-none focus:ring-1 focus:ring-primary/50"
                placeholder="Alias (e.g. ds-v4)"
                autoFocus
              />
              <button
                onClick={handleSaveAliasId}
                className="rounded p-0.5 text-green-500 hover:bg-green-500/10"
                title="Save alias"
              >
                <span className="material-symbols-outlined text-sm">check</span>
              </button>
              <button
                onClick={handleCancelAliasId}
                className="rounded p-0.5 text-text-muted hover:bg-sidebar"
                title="Cancel"
              >
                <span className="material-symbols-outlined text-sm">close</span>
              </button>
            </div>
          ) : null}

          {/* Read-only row when neither field is being edited */}
          {!editingDisplayName && !editingAliasId ? (
            <>
              <p className="text-sm font-medium truncate">{showAs}</p>
              {displayName && (
                <code className="text-[10px] text-text-muted/50 font-mono">{modelId}</code>
              )}
              {/* Display Name badge button */}
              <button
                onClick={() => {
                  setDisplayNameValue(displayName || "");
                  setEditingDisplayName(true);
                }}
                className="rounded p-0.5 text-text-muted hover:bg-sidebar hover:text-primary transition-all"
                title={displayName ? "Edit display name" : "Set display name"}
              >
                <span className="material-symbols-outlined text-sm">badge</span>
              </button>
              {/* Alias indicator + button — directly right of display name badge */}
              {aliasId ? (
                <>
                  <code className="truncate rounded bg-sidebar px-1.5 py-0.5 font-mono text-[10px] text-primary">
                    {aliasId}
                  </code>
                  <button
                    onClick={() => {
                      setAliasIdValue(aliasId || "");
                      setEditingAliasId(true);
                    }}
                    className="rounded p-0.5 text-text-muted hover:bg-sidebar hover:text-primary transition-all"
                    title="Edit alias"
                  >
                    <span className="material-symbols-outlined text-sm">link</span>
                  </button>
                </>
              ) : (
                <button
                  onClick={() => {
                    setAliasIdValue("");
                    setEditingAliasId(true);
                  }}
                  className="rounded p-0.5 text-text-muted hover:bg-sidebar hover:text-primary transition-all"
                  title="Set alias"
                >
                  <span className="material-symbols-outlined text-sm">link</span>
                </button>
              )}
            </>
          ) : null}
        </div>

        {/* Model controller-row: copy + test */}
        <div className="flex items-center gap-1 mt-1">
          <code className="text-xs text-text-muted font-mono bg-sidebar px-1.5 py-0.5 rounded">{fullModel}</code>
          <div className="relative group/btn">
            <button
              onClick={() => onCopy(aliasId || showAs, `model-${modelId}`)}
              className="p-0.5 hover:bg-sidebar rounded text-text-muted hover:text-primary"
            >
              <span className="material-symbols-outlined text-sm">
                {copied === `model-${modelId}` ? "check" : "content_copy"}
              </span>
            </button>
            <span className="pointer-events-none absolute top-5 left-1/2 -translate-x-1/2 text-[10px] text-text-muted whitespace-nowrap opacity-0 group-hover/btn:opacity-100 transition-opacity">
              {copied === `model-${modelId}` ? "Copied!" : "Copy"}
            </span>
          </div>
          {onTest && (
            <div className="relative group/btn">
              <button
                onClick={onTest}
                disabled={isTesting}
                className="p-0.5 hover:bg-sidebar rounded text-text-muted hover:text-primary transition-colors"
              >
                <span className="material-symbols-outlined text-sm" style={isTesting ? { animation: "spin 1s linear infinite" } : undefined}>
                  {isTesting ? "progress_activity" : "science"}
                </span>
              </button>
              <span className="pointer-events-none absolute top-5 left-1/2 -translate-x-1/2 text-[10px] text-text-muted whitespace-nowrap opacity-0 group-hover/btn:opacity-100 transition-opacity">
                {isTesting ? "Testing..." : "Test"}
              </span>
            </div>
          )}
        </div>
      </div>
      <button
        onClick={onDeleteAlias}
        className="p-1 hover:bg-red-50 rounded text-red-500"
        title="Remove model"
      >
        <span className="material-symbols-outlined text-sm">delete</span>
      </button>
    </div>
  );
}

CompatibleModelRow.propTypes = {
  modelId: PropTypes.string.isRequired,
  fullModel: PropTypes.string.isRequired,
  copied: PropTypes.string,
  onCopy: PropTypes.func.isRequired,
  onDeleteAlias: PropTypes.func.isRequired,
  onTest: PropTypes.func,
  testStatus: PropTypes.oneOf(["ok", "error"]),
  isTesting: PropTypes.bool,
  displayName: PropTypes.string,
  onSetDisplayName: PropTypes.func,
  aliasId: PropTypes.string,
  onSetAliasId: PropTypes.func,
};

export default function CompatibleModelsSection({ providerStorageAlias, providerDisplayAlias, modelAliases, customModels, copied, onCopy, onDeleteAlias, onAddCustomModel, onDeleteCustomModel, connections, isAnthropic, modelDisplayNames, onSetDisplayName, onSetAliasId }) {
  const [newModel, setNewModel] = useState("");
  const [adding, setAdding] = useState(false);
  const [importing, setImporting] = useState(false);
  const [testingModelId, setTestingModelId] = useState(null);
  const [modelTestResults, setModelTestResults] = useState({});

  const handleTestModel = async (modelId) => {
    if (testingModelId) return;
    setTestingModelId(modelId);
    try {
      const res = await fetch("/api/models/test", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ model: `${providerStorageAlias}/${modelId}` }),
      });
      const data = await res.json();
      setModelTestResults((prev) => ({ ...prev, [modelId]: data.ok ? "ok" : "error" }));
    } catch {
      setModelTestResults((prev) => ({ ...prev, [modelId]: "error" }));
    } finally {
      setTestingModelId(null);
    }
  };

  const allModels = getProviderCustomModelRows({
    customModels,
    modelAliases,
    providerAlias: providerStorageAlias,
    type: "llm",
  });

  const handleAdd = async () => {
    if (!newModel.trim() || adding) return;
    const modelId = newModel.trim();
    if (allModels.some((model) => model.id === modelId)) {
      alert("Model already exists for this provider.");
      return;
    }

    setAdding(true);
    try {
      await onAddCustomModel(modelId);
      setNewModel("");
    } catch (error) {
      console.log("Error adding model:", error);
    } finally {
      setAdding(false);
    }
  };

  const handleImport = async () => {
    if (importing) return;
    const activeConnection = connections.find((conn) => conn.isActive !== false);
    if (!activeConnection) return;

    setImporting(true);
    try {
      const res = await fetch(`/api/providers/${activeConnection.id}/models`);
      const data = await res.json();
      if (!res.ok) {
        alert(data.error || "Failed to import models");
        return;
      }
      const models = data.models || [];
      if (models.length === 0) {
        alert("No models returned from /models.");
        return;
      }
      let importedCount = 0;
      for (const model of models) {
        const modelId = model.id || model.name || model.model;
        if (!modelId) continue;
        if (allModels.some((entry) => entry.id === modelId)) continue;
        await onAddCustomModel(modelId);
        importedCount += 1;
      }
      if (importedCount === 0) {
        alert("No new models were added.");
      }
    } catch (error) {
      console.log("Error importing models:", error);
    } finally {
      setImporting(false);
    }
  };

  const canImport = connections.some((conn) => conn.isActive !== false);

  return (
    <div className="flex flex-col gap-4">
      <p className="text-sm text-text-muted">
        Add {isAnthropic ? "Anthropic" : "OpenAI"}-compatible models manually or import them from the /models endpoint.
      </p>

      <div className="flex items-end gap-2 flex-wrap">
        <div className="flex-1 min-w-[240px]">
          <label htmlFor="new-compatible-model-input" className="text-xs text-text-muted mb-1 block">Model ID</label>
          <input
            id="new-compatible-model-input"
            type="text"
            value={newModel}
            onChange={(e) => setNewModel(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && handleAdd()}
            placeholder={isAnthropic ? "claude-3-opus-20240229" : "gpt-4o"}
            className="w-full px-3 py-2 text-sm border border-border rounded-lg bg-background focus:outline-none focus:border-primary"
          />
        </div>
        <Button size="sm" icon="add" onClick={handleAdd} disabled={!newModel.trim() || adding}>
          {adding ? "Adding..." : "Add"}
        </Button>
        <Button size="sm" variant="secondary" icon="download" onClick={handleImport} disabled={!canImport || importing}>
          {importing ? "Importing..." : "Import from /models"}
        </Button>
      </div>

      {!canImport && (
        <p className="text-xs text-text-muted">
          Add a connection to enable importing models.
        </p>
      )}

      {allModels.length > 0 && (
        <div className="flex flex-col gap-3">
          {allModels.map(({ id, alias, source }) => {
            const fullModelId = `${providerDisplayAlias}/${id}`;
            return (
              <CompatibleModelRow
                key={`${source}-${providerStorageAlias}/${id}`}
                modelId={id}
                fullModel={fullModelId}
                copied={copied}
                onCopy={onCopy}
                onDeleteAlias={() => {
                  if (source === "custom") {
                    // If this custom model has an alias, remove the alias but keep the model
                    if (alias) {
                      onDeleteAlias(alias);
                    } else {
                      onDeleteCustomModel(id);
                    }
                  } else {
                    onDeleteAlias(alias);
                  }
                }}
                onTest={connections.length > 0 ? () => handleTestModel(id) : undefined}
                testStatus={modelTestResults[id]}
                isTesting={testingModelId === id}
                displayName={modelDisplayNames?.[fullModelId]}
                onSetDisplayName={onSetDisplayName}
                aliasId={alias}
                onSetAliasId={onSetAliasId}
              />
            );
          })}
        </div>
      )}
    </div>
  );
}

CompatibleModelsSection.propTypes = {
  providerStorageAlias: PropTypes.string.isRequired,
  providerDisplayAlias: PropTypes.string.isRequired,
  modelAliases: PropTypes.object.isRequired,
  customModels: PropTypes.arrayOf(PropTypes.object),
  copied: PropTypes.string,
  onCopy: PropTypes.func.isRequired,
  onDeleteAlias: PropTypes.func.isRequired,
  onAddCustomModel: PropTypes.func.isRequired,
  onDeleteCustomModel: PropTypes.func.isRequired,
  connections: PropTypes.arrayOf(PropTypes.shape({
    id: PropTypes.string,
    isActive: PropTypes.bool,
  })).isRequired,
  isAnthropic: PropTypes.bool,
  modelDisplayNames: PropTypes.object,
  onSetDisplayName: PropTypes.func,
  onSetAliasId: PropTypes.func,
};
