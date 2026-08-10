import { urlOrigin } from "../../lib/urlRelay";

interface Props {
  /** Already validated by `sanitizeRelayUrl` — this component never opens it. */
  url: string;
  /** Heading above the URL. Says why the toast appeared. */
  label?: string;
  onOpen: () => void;
  onDismiss: () => void;
}

/**
 * Confirmation prompt for a URL something inside the container wants opened in
 * the host browser.
 *
 * The origin is rendered separately from the rest of the URL and is never
 * truncated. A single `nowrap`/`ellipsis` line looks tidy but is a spoofing
 * primitive: `https://accounts.example.com/....(600 chars)....@evil.tld/` shows
 * the reassuring half and hides the half that decides where the request goes.
 * `sanitizeRelayUrl` already rejects the userinfo form; showing the origin in
 * full is the belt to that braces, and it also covers the plainer case of a
 * long path pushing the host out of view.
 *
 * Render this with a `key` that changes whenever the URL does. The prompt slot
 * is shared and long-lived, so without one React mutates the node in place: the
 * text swaps with no animation, and a user reading URL A can click Open on URL
 * B that arrived a second later.
 */
export default function UrlToast({
  url,
  label = "Long URL detected",
  onOpen,
  onDismiss,
}: Props) {
  const origin = urlOrigin(url);
  const rest = origin && url.startsWith(origin) ? url.slice(origin.length) : url;

  return (
    <div
      className="animate-slide-down"
      role="status"
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
          {label}
        </div>
        <div
          data-testid="url-toast-url"
          title={url}
          style={{
            fontSize: 12,
            fontFamily: "monospace",
            color: "var(--text-primary)",
            display: "flex",
            alignItems: "baseline",
            minWidth: 0,
          }}
        >
          {origin && (
            <span
              data-testid="url-toast-origin"
              style={{
                fontWeight: 700,
                // The part that decides where the credentials go. It wraps
                // rather than truncates, whatever else has to give.
                flexShrink: 0,
                overflowWrap: "anywhere",
              }}
            >
              {origin}
            </span>
          )}
          <span
            data-testid="url-toast-rest"
            style={{
              color: "var(--text-secondary)",
              overflow: "hidden",
              textOverflow: "ellipsis",
              whiteSpace: "nowrap",
              minWidth: 0,
            }}
          >
            {rest}
          </span>
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
