import type { KeyboardEvent } from "react";
import { isAnthropicSignInUrl, urlOrigin } from "../../lib/urlRelay";
import Button from "../ui/Button";

/**
 * Marks the toast's subtree. `TerminalView` uses it to answer "is focus inside
 * the thing I am about to unmount?", which is what decides whether dismissing
 * has to hand focus back to the terminal.
 */
export const URL_TOAST_SELECTOR = '[data-testid="url-toast"]';

/**
 * The chord that jumps from the terminal into this toast.
 *
 * Bound in `TerminalView` on `document` in the capture phase, the same way
 * `useKeyboardShortcuts` binds the app's other chords, because xterm would
 * otherwise forward it to the shell. Shift is what keeps it clear of the
 * terminal: plain Ctrl+O is readline's `operate-and-get-next`.
 */
export const URL_TOAST_SHORTCUT = "Ctrl+Shift+O";

/**
 * Marks the *default* action inside the toast, so the owner can put focus
 * there without a ref threaded through `ui/Button` — which is a plain function
 * component and not this file's to change. Which button it is depends on the
 * URL (see the sign-in note below), so the attribute moves with the decision
 * rather than the caller having to repeat it.
 */
export const URL_TOAST_PRIMARY_SELECTOR = '[data-url-toast-primary="true"]';

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
 *
 * ## Reachable without a mouse, and it does not take focus to manage it
 *
 * This toast is the only route to completing a sign-in started in a terminal,
 * and it used to be mouse-only: xterm's helper textarea swallows Tab, so there
 * was no way to reach these buttons at all from the keyboard.
 *
 * The obvious fix — focus the default action when the toast appears — was
 * rejected on two counts. The terminal underneath is *live*: the user may be
 * mid-command, and every keystroke after the steal would go to a button instead
 * of the shell. Worse, the default action opens a URL chosen by the untrusted
 * side of the sandbox, and a focused button is one stray Space or Enter away
 * from doing it. This prompt exists precisely to make that a deliberate act.
 *
 * So focus stays where the user put it and the toast is reachable on demand:
 * {@link URL_TOAST_SHORTCUT} jumps to the default action (the hint is on
 * screen, next to the label, because a shortcut nobody is told about is not a
 * route), Tab then moves between the actions normally — this subtree is not
 * inside xterm — and Escape dismisses. Escape is handled *here*, on the
 * toast's own subtree, rather than globally: Escape belongs to whatever is
 * running in the terminal, and a document-level binding for it would break vim
 * for everyone who never looked at this toast.
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

  // `Button` already owns the filled/outlined variants — including the rule
  // that filled uses `--accent-emphasis` and never `--accent`, which is the
  // foreground/link accent and fails WCAG AA behind white text.
  const hostButton = (
    <Button
      variant={signIn ? "secondary" : "primary"}
      data-url-toast-primary={signIn ? undefined : "true"}
      onClick={onOpen}
      className="flex-shrink-0"
      title={
        signIn
          ? "Open in your own browser instead — the callback then has to reach the container by some other route"
          : undefined
      }
    >
      Open
    </Button>
  );

  const containerButton = onOpenInContainer && (
    // A sign-in completed in the *container's* browser lands its callback on
    // the container's own loopback, which is where the tool waiting for it is
    // listening — no host round trip, no auth bridge.
    <Button
      variant={signIn ? "primary" : "secondary"}
      data-url-toast-primary={signIn ? "true" : undefined}
      onClick={onOpenInContainer}
      className="flex-shrink-0"
      title="Open in a browser inside the container, and watch it in the Browser tab"
    >
      In container
    </Button>
  );

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.key !== "Escape") return;
    // Scoped to this subtree, so the terminal's own Escape is untouched.
    e.preventDefault();
    e.stopPropagation();
    onDismiss();
  };

  return (
    <div
      className="animate-slide-down"
      data-testid="url-toast"
      role="status"
      aria-atomic="true"
      aria-keyshortcuts="Control+Shift+O"
      onKeyDown={onKeyDown}
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
        boxShadow: "var(--shadow-overlay)",
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
          {" · "}
          <span data-testid="url-toast-shortcut" style={{ fontFamily: "monospace" }}>
            {URL_TOAST_SHORTCUT}
          </span>{" "}
          to reach the buttons, Esc to dismiss
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

      <Button
        variant="ghost"
        onClick={onDismiss}
        className="flex-shrink-0"
        aria-label="Dismiss"
        title="Dismiss (Esc)"
      >
        ✕
      </Button>
    </div>
  );
}
