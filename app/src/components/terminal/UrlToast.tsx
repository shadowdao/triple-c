import type { CSSProperties, MouseEvent } from "react";
import { isAnthropicSignInUrl, urlOrigin } from "../../lib/urlRelay";

interface Props {
  /** Already validated by `sanitizeRelayUrl` — this component never opens it. */
  url: string;
  /** Heading above the URL. Says why the toast appeared. */
  label?: string;
  onOpen: () => void;
  /** Open it in the container's own browser instead of the host's. Omitted when
   *  the project has no browser to open it in. */
  onOpenInContainer?: () => void;
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
 *
 * ## Anthropic sign-in links default to the container's browser
 *
 * For an ordinary URL the host browser is the right answer and stays the
 * default. For a sign-in it is the *wrong* one: the callback listener the CLI
 * is waiting on is inside the container, so a host browser completes the sign-in
 * and then posts the result somewhere nothing is listening, and the terminal
 * hangs until it times out. Making the host button primary there was quietly
 * steering every user into that. The container-side browser closes the loop
 * with no host round trip and no auth bridge, so it leads — and the host button
 * stays, because a user who has the auth bridge on, or who wants their existing
 * browser session, still needs it.
 */
export default function UrlToast({
  url,
  label = "Long URL detected",
  onOpen,
  onOpenInContainer,
  onDismiss,
}: Props) {
  const origin = urlOrigin(url);
  const rest = origin && url.startsWith(origin) ? url.slice(origin.length) : url;
  // Only when there is somewhere to send it: without `onOpenInContainer` the
  // host button is the only action there is, so it stays primary.
  const signIn = !!onOpenInContainer && isAnthropicSignInUrl(url);

  // Filled uses `--accent-emphasis`, never `--accent` — the latter is the
  // foreground/link accent and fails WCAG AA behind white text.
  const primaryStyle: CSSProperties = {
    padding: "4px 12px",
    fontSize: 12,
    fontWeight: 600,
    color: "#fff",
    background: "var(--accent-emphasis)",
    border: "1px solid transparent",
    borderRadius: 4,
    cursor: "pointer",
    whiteSpace: "nowrap",
    flexShrink: 0,
  };
  const secondaryStyle: CSSProperties = {
    padding: "4px 10px",
    fontSize: 12,
    fontWeight: 600,
    color: "var(--text-primary)",
    background: "transparent",
    border: "1px solid var(--border-color)",
    borderRadius: 4,
    cursor: "pointer",
    whiteSpace: "nowrap",
    flexShrink: 0,
  };

  /** Hover feedback for whichever button is currently the filled one. */
  const hover = (primary: boolean) =>
    primary
      ? {
          onMouseEnter: (e: MouseEvent<HTMLButtonElement>) =>
            (e.currentTarget.style.background = "var(--accent-emphasis-hover)"),
          onMouseLeave: (e: MouseEvent<HTMLButtonElement>) =>
            (e.currentTarget.style.background = "var(--accent-emphasis)"),
        }
      : {
          onMouseEnter: (e: MouseEvent<HTMLButtonElement>) =>
            (e.currentTarget.style.background = "var(--bg-tertiary)"),
          onMouseLeave: (e: MouseEvent<HTMLButtonElement>) =>
            (e.currentTarget.style.background = "transparent"),
        };

  const hostButton = (
    <button
      onClick={onOpen}
      title={
        signIn
          ? "Open in your own browser instead — the callback then has to reach the container by some other route"
          : undefined
      }
      style={signIn ? secondaryStyle : primaryStyle}
      {...hover(!signIn)}
    >
      Open
    </button>
  );

  const containerButton = onOpenInContainer && (
    // A sign-in completed in the *container's* browser lands its callback on
    // the container's own loopback, which is where the tool waiting for it is
    // listening — no host round trip, no auth bridge.
    <button
      onClick={onOpenInContainer}
      title="Open in a browser inside the container, and watch it in the Browser tab"
      style={signIn ? primaryStyle : secondaryStyle}
      {...hover(signIn)}
    >
      In container
    </button>
  );

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
        {signIn && (
          <div
            data-testid="url-toast-signin-hint"
            style={{
              marginTop: 3,
              fontSize: 11,
              color: "var(--text-secondary)",
              lineHeight: 1.35,
            }}
          >
            Sign-in link — the callback listener is inside the container.
            Opening it there closes the loop; the host browser needs the auth
            bridge.
          </div>
        )}
      </div>

      {signIn ? (
        <>
          {containerButton}
          {hostButton}
        </>
      ) : (
        <>
          {hostButton}
          {containerButton}
        </>
      )}

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
