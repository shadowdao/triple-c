interface Props {
  url: string;
  onOpen: () => void;
  onDismiss: () => void;
}

export default function UrlToast({ url, onOpen, onDismiss }: Props) {
  return (
    <div
      className="animate-slide-down"
      style={{
        position: "absolute",
        top: 12,
        left: "50%",
        transform: "translateX(-50%)",
        zIndex: 40,
        display: "flex",
        alignItems: "center",
        gap: 10,
        padding: "8px 12px",
        background: "var(--bg-secondary)",
        border: "1px solid var(--border-color)",
        borderRadius: 8,
        boxShadow: "0 4px 12px rgba(0,0,0,0.4)",
        maxWidth: "min(90%, 600px)",
      }}
    >
      <div style={{ flex: 1, minWidth: 0 }}>
        <div
          style={{
            fontSize: 12,
            color: "var(--text-secondary)",
            marginBottom: 2,
          }}
        >
          Long URL detected
        </div>
        <div
          style={{
            fontSize: 12,
            fontFamily: "monospace",
            color: "var(--text-primary)",
            overflow: "hidden",
            textOverflow: "ellipsis",
            whiteSpace: "nowrap",
          }}
        >
          {url}
        </div>
      </div>

      <button
        onClick={onOpen}
        style={{
          padding: "4px 12px",
          fontSize: 12,
          fontWeight: 600,
          color: "#fff",
          background: "var(--accent)",
          border: "none",
          borderRadius: 4,
          cursor: "pointer",
          whiteSpace: "nowrap",
          flexShrink: 0,
        }}
        onMouseEnter={(e) =>
          (e.currentTarget.style.background = "var(--accent-hover)")
        }
        onMouseLeave={(e) =>
          (e.currentTarget.style.background = "var(--accent)")
        }
      >
        Open
      </button>

      <button
        onClick={onDismiss}
        style={{
          padding: "2px 6px",
          fontSize: 14,
          lineHeight: 1,
          color: "var(--text-secondary)",
          background: "transparent",
          border: "none",
          borderRadius: 4,
          cursor: "pointer",
          flexShrink: 0,
        }}
        onMouseEnter={(e) =>
          (e.currentTarget.style.color = "var(--text-primary)")
        }
        onMouseLeave={(e) =>
          (e.currentTarget.style.color = "var(--text-secondary)")
        }
        aria-label="Dismiss"
      >
        ✕
      </button>
    </div>
  );
}
