"use client";

import { useState } from "react";
import PropTypes from "prop-types";
import { Button, Input, Modal } from "@/shared/components";

const TEMPLATE_HELP = `Available templates:
{uuid} → full UUID with dashes
{uuid-nodash} → UUID without dashes
{timestamp} → Unix timestamp (milliseconds)
{timestamp-sec} → Unix timestamp (seconds)
{random-hex-8} → 8-character hex
{random-hex-16} → 16-character hex
{random-hex-32} → 32-character hex
{iso8601} → ISO 8601 timestamp

Examples:
X-Request-ID: {uuid-nodash}
traceparent: 00-{uuid-nodash}-{random-hex-16}-01
User-Agent: MyClient/1.0.0`;

export default function CustomHeadersModal({ isOpen, headers = {}, blacklist = [], onSave, onClose }) {
  const [customHeaders, setCustomHeaders] = useState(() => {
    return Object.entries(headers).map(([key, value]) => ({ key, value }));
  });
  const [headerBlacklist, setHeaderBlacklist] = useState(() => blacklist.join(", "));
  const [showHelp, setShowHelp] = useState(false);

  const addHeader = () => {
    setCustomHeaders([...customHeaders, { key: "", value: "" }]);
  };

  const removeHeader = (index) => {
    setCustomHeaders(customHeaders.filter((_, i) => i !== index));
  };

  const updateHeader = (index, field, value) => {
    const updated = [...customHeaders];
    updated[index][field] = value;
    setCustomHeaders(updated);
  };

  const handleSave = () => {
    const headersObj = {};
    for (const { key, value } of customHeaders) {
      if (key.trim()) {
        headersObj[key.trim()] = value;
      }
    }
    const blacklistArr = headerBlacklist
      .split(",")
      .map((h) => h.trim())
      .filter(Boolean);

    onSave({ customHeaders: headersObj, headerBlacklist: blacklistArr });
  };

  return (
    <Modal isOpen={isOpen} title="Custom Headers" onClose={onClose} size="large">
      <div className="flex flex-col gap-4">
        <div className="text-sm text-text-muted">
          Add custom headers to inject before provider requests. Use templates for dynamic values.
          <button
            type="button"
            onClick={() => setShowHelp(!showHelp)}
            className="ml-2 text-primary hover:underline"
          >
            {showHelp ? "Hide" : "Show"} template syntax
          </button>
        </div>

        {showHelp && (
          <pre className="text-xs bg-surface-elevated p-3 rounded border border-border overflow-x-auto whitespace-pre-wrap">
            {TEMPLATE_HELP}
          </pre>
        )}

        <div className="border-t border-border pt-4">
          <div className="flex items-center justify-between mb-2">
            <span className="text-sm font-medium">Custom Headers</span>
            <Button onClick={addHeader} variant="secondary" size="sm">
              Add Header
            </Button>
          </div>

          <div className="flex flex-col gap-2 max-h-80 overflow-y-auto">
            {customHeaders.length === 0 ? (
              <div className="text-sm text-text-muted text-center py-4">
                No custom headers. Click &quot;Add Header&quot; to create one.
              </div>
            ) : (
              customHeaders.map((header, index) => (
                <div key={index} className="flex gap-2 items-start">
                  <Input
                    placeholder="Header-Name"
                    value={header.key}
                    onChange={(e) => updateHeader(index, "key", e.target.value)}
                    className="flex-1"
                  />
                  <Input
                    placeholder="value or {template}"
                    value={header.value}
                    onChange={(e) => updateHeader(index, "value", e.target.value)}
                    className="flex-1"
                  />
                  <Button
                    onClick={() => removeHeader(index)}
                    variant="ghost"
                    size="sm"
                    className="mt-1"
                  >
                    Remove
                  </Button>
                </div>
              ))
            )}
          </div>
        </div>

        <div className="border-t border-border pt-4">
          <Input
            label="Header Blacklist"
            value={headerBlacklist}
            onChange={(e) => setHeaderBlacklist(e.target.value)}
            placeholder="x-stainless-lang, x-app, user-agent"
            hint="Comma-separated header names to remove after all headers are set (case-insensitive)."
          />
        </div>

        <div className="flex gap-2 border-t border-border pt-4">
          <Button onClick={handleSave} fullWidth>
            Save
          </Button>
          <Button onClick={onClose} variant="ghost" fullWidth>
            Cancel
          </Button>
        </div>
      </div>
    </Modal>
  );
}

CustomHeadersModal.propTypes = {
  isOpen: PropTypes.bool.isRequired,
  headers: PropTypes.object,
  blacklist: PropTypes.array,
  onSave: PropTypes.func.isRequired,
  onClose: PropTypes.func.isRequired,
};
