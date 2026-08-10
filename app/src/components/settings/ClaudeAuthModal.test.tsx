import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor, act } from "@testing-library/react";
import ClaudeAuthModal from "./ClaudeAuthModal";

const acquireClaudeToken = vi.fn();
const submitClaudeTokenCode = vi.fn();

vi.mock("../../lib/tauri-commands", () => ({
  acquireClaudeToken: (...args: unknown[]) => acquireClaudeToken(...args),
  submitClaudeTokenCode: (...args: unknown[]) => submitClaudeTokenCode(...args),
  hasClaudeToken: vi.fn(),
  clearClaudeToken: vi.fn(),
  cancelClaudeToken: (...args: unknown[]) => cancelClaudeToken(...args),
}));

const cancelClaudeToken = vi.fn(() => Promise.resolve());

const openUrl = vi.fn();
vi.mock("@tauri-apps/plugin-opener", () => ({
  openUrl: (...args: unknown[]) => openUrl(...args),
}));

/** Captured event handlers, keyed by event name, so tests can emit. */
const handlers = new Map<string, (event: { payload: unknown }) => void>();
const unlisten = vi.fn();

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, handler: (e: { payload: unknown }) => void) => {
    handlers.set(name, handler);
    return unlisten;
  }),
}));

/** Every event the hook subscribes to, so the unmount test counts the right
 *  number of teardowns instead of a magic number that drifts. */
const EVENT_NAMES = [
  "claude-token-progress",
  "claude-token-output",
  "claude-token-link",
  "claude-token-code-rejected",
];

/** The sign-in URL at its real length (346 characters, measured against
 *  Claude Code 2.1.226) and the 80-column slice of it that is all the visible
 *  transcript ever contains. */
const FULL_URL =
  "https://claude.com/cai/oauth/authorize?code=true&client_id=9d1c250a-e61b-44d9-88ed-5944d1962f5e&response_type=code&redirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback&scope=user%3Ainference&code_challenge=RUX5MlWvwld1dmpvF_aPIJQWMBmffuJt4dOdL13zWAg&code_challenge_method=S256&state=su-x9PgZzvkBd3-um6G1llLNDgxptyO6HERvvCSrTbg";
const TRUNCATED_URL = FULL_URL.slice(0, 80);

function emitOutput(chunk: string, projectId = "p1") {
  act(() => {
    handlers.get("claude-token-output")?.({
      payload: { project_id: projectId, chunk },
    });
  });
}

function emitLink(url: string, projectId = "p1") {
  act(() => {
    handlers.get("claude-token-link")?.({
      payload: { project_id: projectId, url },
    });
  });
}

function emitCodeRejected(message: string, attemptsRemaining: number) {
  act(() => {
    handlers.get("claude-token-code-rejected")?.({
      payload: {
        project_id: "p1",
        message,
        attempts_remaining: attemptsRemaining,
      },
    });
  });
}

function renderModal(
  overrides: { onClose?: () => void; onAuthenticated?: () => void } = {},
) {
  return render(
    <ClaudeAuthModal
      projectId="p1"
      projectName="api-server"
      onClose={overrides.onClose ?? vi.fn()}
      onAuthenticated={overrides.onAuthenticated ?? vi.fn()}
    />,
  );
}

/** Both listeners register before `acquire_claude_token` is invoked. */
async function flowStarted() {
  await waitFor(() => expect(acquireClaudeToken).toHaveBeenCalledWith("p1"));
}

describe("ClaudeAuthModal", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    handlers.clear();
    // A flow that never resolves on its own — the CLI is sitting on its prompt.
    acquireClaudeToken.mockImplementation(() => new Promise(() => {}));
    submitClaudeTokenCode.mockResolvedValue(undefined);
  });

  it("starts the flow for the given project", async () => {
    renderModal();
    await flowStarted();
  });

  it("submits the pasted code to the backend", async () => {
    renderModal();
    await flowStarted();

    fireEvent.change(screen.getByLabelText("Authentication code"), {
      target: { value: " code-123 " },
    });
    fireEvent.click(screen.getByRole("button", { name: "Submit code" }));

    // Trimmed on the way out — the backend rejects surrounding whitespace noise.
    await waitFor(() =>
      expect(submitClaudeTokenCode).toHaveBeenCalledWith("code-123"),
    );
    await waitFor(() =>
      expect(screen.getByLabelText("Authentication code")).toHaveValue(""),
    );
  });

  it("submits on Enter as well as on the button", async () => {
    renderModal();
    await flowStarted();

    const input = screen.getByLabelText("Authentication code");
    fireEvent.change(input, { target: { value: "code-456" } });
    fireEvent.submit(input.closest("form")!);

    await waitFor(() =>
      expect(submitClaudeTokenCode).toHaveBeenCalledWith("code-456"),
    );
  });

  it("refuses an empty code without calling the backend", async () => {
    renderModal();
    await flowStarted();

    fireEvent.click(screen.getByRole("button", { name: "Submit code" }));

    await screen.findByText("Enter the code shown after signing in.");
    expect(submitClaudeTokenCode).not.toHaveBeenCalled();
  });

  it("reports a backend rejection instead of dumping the raw value", async () => {
    submitClaudeTokenCode.mockRejectedValue(
      "That code contains invalid characters. Copy it again and retry.",
    );
    renderModal();
    await flowStarted();

    fireEvent.change(screen.getByLabelText("Authentication code"), {
      target: { value: "bad" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Submit code" }));

    await screen.findByText(
      "That code contains invalid characters. Copy it again and retry.",
    );
  });

  it("linkifies the sign-in URL from the streamed output and opens it in the host browser", async () => {
    renderModal();
    await flowStarted();

    const url = "https://claude.ai/oauth/authorize?code=true&client_id=abc";
    emitOutput(`Use this url to sign in:\n${url}\n`);

    const link = await screen.findByRole("link", { name: url });
    fireEvent.click(link);
    await waitFor(() => expect(openUrl).toHaveBeenCalledWith(url));
  });

  it("ignores output belonging to a different project", async () => {
    renderModal();
    await flowStarted();

    emitOutput("https://claude.ai/oauth/authorize?code=other", "p2");
    expect(screen.getByTestId("claude-auth-output")).not.toHaveTextContent(
      "code=other",
    );
  });

  it("surfaces an actionable failure when the flow ends badly", async () => {
    acquireClaudeToken.mockRejectedValue(
      "`claude setup-token` finished but printed no recognisable token. Nothing was stored.",
    );
    renderModal();

    const banner = await screen.findByTestId("claude-auth-error");
    expect(banner).toHaveTextContent(/printed no recognisable token/);
  });

  it("announces success and notifies the caller", async () => {
    acquireClaudeToken.mockResolvedValue(undefined);
    const onAuthenticated = vi.fn();
    renderModal({ onAuthenticated });

    await screen.findByTestId("claude-auth-success");
    expect(onAuthenticated).toHaveBeenCalledTimes(1);
  });

  it("confirms before cancelling, then aborts the container-side CLI", async () => {
    const onClose = vi.fn();
    renderModal({ onClose });
    await flowStarted();

    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(screen.getByText(/no token is stored/i)).toBeInTheDocument();
    // Confirming is required — the first click must not cancel anything.
    expect(cancelClaudeToken).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "Cancel sign-in" }));
    await waitFor(() => expect(cancelClaudeToken).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(onClose).toHaveBeenCalled());
  });

  it("still closes when the cancel command rejects", async () => {
    cancelClaudeToken.mockRejectedValueOnce(new Error("nope"));
    const onClose = vi.fn();
    renderModal({ onClose });
    await flowStarted();

    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    fireEvent.click(screen.getByRole("button", { name: "Cancel sign-in" }));
    await waitFor(() => expect(onClose).toHaveBeenCalled());
  });

  it("removes its event listeners on unmount", async () => {
    const { unmount } = renderModal();
    await flowStarted();
    unmount();
    await waitFor(() =>
      expect(unlisten).toHaveBeenCalledTimes(EVENT_NAMES.length),
    );
  });

  // ── The hyperlink target, not the wrapped display text ────────────────
  //
  // `claude setup-token` slices the *visible* text of its OSC 8 hyperlink to
  // the terminal width, so the transcript holds five 80-character pieces of a
  // 346-character URL. The backend lifts the whole thing out of the hyperlink
  // parameter and sends it on `claude-token-link`.

  it("prefers the hyperlink target over the wrapped copy in the transcript", async () => {
    renderModal();
    await flowStarted();

    // What the transcript holds: the first slice only.
    emitOutput(`Browser didn't open? Use the url below to sign in\n${TRUNCATED_URL}\n`);
    // What the hyperlink parameter holds: all of it.
    emitLink(FULL_URL);

    const link = await screen.findByRole("link", { name: FULL_URL });
    fireEvent.click(link);
    await waitFor(() => expect(openUrl).toHaveBeenCalledWith(FULL_URL));
    expect(openUrl).not.toHaveBeenCalledWith(TRUNCATED_URL);
  });

  it("refuses a hyperlink target that is not an Anthropic sign-in address", async () => {
    renderModal();
    await flowStarted();

    emitLink("https://evil.tld/cai/oauth/authorize?code=true");

    expect(screen.queryByRole("link")).not.toBeInTheDocument();
    expect(openUrl).not.toHaveBeenCalled();
  });

  it("ignores a hyperlink belonging to a different project", async () => {
    renderModal();
    await flowStarted();

    emitLink(FULL_URL, "p2");
    expect(screen.queryByRole("link")).not.toBeInTheDocument();
  });

  // ── A refused code is recoverable, not a hang ─────────────────────────

  it("reports a rejected code and lets another one be submitted", async () => {
    renderModal();
    await flowStarted();

    const input = screen.getByLabelText("Authentication code");
    fireEvent.change(input, { target: { value: "truncated" } });
    fireEvent.click(screen.getByRole("button", { name: "Submit code" }));
    await waitFor(() =>
      expect(submitClaudeTokenCode).toHaveBeenCalledWith("truncated"),
    );
    // Before the rejection arrives the UI claims the sign-in is completing.
    expect(screen.getByText("Finishing sign-in")).toBeInTheDocument();

    emitCodeRejected(
      "That code was rejected — `claude setup-token` reports the full code was not copied. Copy it again from the Anthropic page and submit it; 2 attempts left.",
      2,
    );

    // Reported, not waited out — and the flow is still live.
    await screen.findByText(/That code was rejected/);
    expect(screen.getByText("Code rejected — try again")).toBeInTheDocument();
    expect(screen.queryByText("Finishing sign-in")).not.toBeInTheDocument();
    expect(screen.queryByTestId("claude-auth-error")).not.toBeInTheDocument();

    // A second code goes through without restarting the whole flow.
    fireEvent.change(input, { target: { value: "the-whole-code" } });
    fireEvent.click(screen.getByRole("button", { name: "Submit code" }));
    await waitFor(() =>
      expect(submitClaudeTokenCode).toHaveBeenLastCalledWith("the-whole-code"),
    );
    expect(acquireClaudeToken).toHaveBeenCalledTimes(1);
  });

  it("ends with a reported failure when the retries run out", async () => {
    acquireClaudeToken.mockRejectedValue(
      "`claude setup-token` rejected the code 3 times, so the sign-in was abandoned. No token was stored.",
    );
    renderModal();

    const banner = await screen.findByTestId("claude-auth-error");
    expect(banner).toHaveTextContent(/rejected the code 3 times/);
  });
});
