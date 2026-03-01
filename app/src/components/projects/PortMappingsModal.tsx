import { useState, useEffect, useRef, useCallback } from "react";
import type { PortMapping } from "../../lib/types";

interface Props {
  portMappings: PortMapping[];
  disabled: boolean;
  onSave: (mappings: PortMapping[]) => Promise<void>;
  onClose: () => void;
}

export default function PortMappingsModal({ portMappings: initial, disabled, onSave, onClose }: Props) {
  const [mappings, setMappings] = useState<PortMapping[]>(initial);
  const overlayRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("keydown", handleKeyDown);
    return () => document.removeEventListener("keydown", handleKeyDown);
  }, [onClose]);

  const handleOverlayClick = useCallback(
    (e: React.MouseEvent<HTMLDivElement>) => {
      if (e.target === overlayRef.current) onClose();
    },
    [onClose],
  );

  const updatePort = (index: number, field: "host_port" | "container_port", value: string) => {
    const updated = [...mappings];
    const num = parseInt(value, 10);
    updated[index] = { ...updated[index], [field]: isNaN(num) ? 0 : num };
    setMappings(updated);
  };

  const updateProtocol = (index: number, value: string) => {
    const updated = [...mappings];
    updated[index] = { ...updated[index], protocol: value };
    setMappings(updated);
  };

  const removeMapping = async (index: number) => {
    const updated = mappings.filter((_, i) => i !== index);
    setMappings(updated);
    try { await onSave(updated); } catch (err) {
      console.error("Failed to remove port mapping:", err);
    }
  };

  const addMapping = async () => {
    const updated = [...mappings, { host_port: 0, container_port: 0, protocol: "tcp" }];
    setMappings(updated);
    try { await onSave(updated); } catch (err) {
      console.error("Failed to add port mapping:", err);
    }
  };

  const handleBlur = async () => {
    try { await onSave(mappings); } catch (err) {
      console.error("Failed to update port mappings:", err);
    }
  };

  return (
    <div
      ref={overlayRef}
      onClick={handleOverlayClick}
      className="fixed inset-0 bg-black/50 flex items-center justify-center z-50"
    >
      <div className="bg-[var(--bg-secondary)] border border-[var(--border-color)] rounded-lg p-6 w-[36rem] shadow-xl max-h-[80vh] overflow-y-auto">
        <h2 className="text-lg font-semibold mb-2">Port Mappings</h2>
        <p className="text-xs text-[var(--text-secondary)] mb-4">
          Map host ports to container ports. Services can be started after the container is running.
        </p>

        {disabled && (
          <div className="px-2 py-1.5 mb-3 bg-[var(--warning)]/15 border border-[var(--warning)]/30 rounded text-xs text-[var(--warning)]">
            Container must be stopped to change port mappings.
          </div>
        )}

        <div className="space-y-2 mb-4">
          {mappings.length === 0 && (
            <p className="text-xs text-[var(--text-secondary)]">No port mappings configured.</p>
          )}
          {mappings.length > 0 && (
            <div className="flex gap-2 items-center text-xs text-[var(--text-secondary)] px-0.5">
              <span className="w-[30%]">Host Port</span>
              <span className="w-[30%]">Container Port</span>
              <span className="w-[25%]">Protocol</span>
              <span className="w-[15%]" />
            </div>
          )}
          {mappings.map((pm, i) => (
            <div key={i} className="flex gap-2 items-center">
              <input
                type="number"
                min="1"
                max="65535"
                value={pm.host_port || ""}
                onChange={(e) => updatePort(i, "host_port", e.target.value)}
                onBlur={handleBlur}
                placeholder="8080"
                disabled={disabled}
                className="w-[30%] px-2 py-1.5 bg-[var(--bg-primary)] border border-[var(--border-color)] rounded text-sm text-[var(--text-primary)] focus:outline-none focus:border-[var(--accent)] disabled:opacity-50 font-mono"
              />
              <input
                type="number"
                min="1"
                max="65535"
                value={pm.container_port || ""}
                onChange={(e) => updatePort(i, "container_port", e.target.value)}
                onBlur={handleBlur}
                placeholder="8080"
                disabled={disabled}
                className="w-[30%] px-2 py-1.5 bg-[var(--bg-primary)] border border-[var(--border-color)] rounded text-sm text-[var(--text-primary)] focus:outline-none focus:border-[var(--accent)] disabled:opacity-50 font-mono"
              />
              <select
                value={pm.protocol}
                onChange={(e) => { updateProtocol(i, e.target.value); handleBlur(); }}
                disabled={disabled}
                className="w-[25%] px-2 py-1.5 bg-[var(--bg-primary)] border border-[var(--border-color)] rounded text-sm text-[var(--text-primary)] focus:outline-none focus:border-[var(--accent)] disabled:opacity-50"
              >
                <option value="tcp">TCP</option>
                <option value="udp">UDP</option>
              </select>
              <button
                onClick={() => removeMapping(i)}
                disabled={disabled}
                className="w-[15%] px-2 py-1.5 text-sm text-[var(--error)] hover:bg-[var(--bg-primary)] rounded disabled:opacity-50 transition-colors text-center"
              >
                x
              </button>
            </div>
          ))}
        </div>

        <div className="flex justify-between items-center">
          <button
            onClick={addMapping}
            disabled={disabled}
            className="text-sm text-[var(--accent)] hover:text-[var(--accent-hover)] disabled:opacity-50 transition-colors"
          >
            + Add port mapping
          </button>
          <button
            onClick={onClose}
            className="px-4 py-2 text-sm text-[var(--text-secondary)] hover:text-[var(--text-primary)] transition-colors"
          >
            Close
          </button>
        </div>
      </div>
    </div>
  );
}
