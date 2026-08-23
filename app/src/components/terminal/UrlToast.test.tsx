import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import UrlToast from "./UrlToast";

/**
 * The toast is the *only* thing standing between a container-chosen URL and
 * the host's browser, so what it shows has to be what will be opened — and the
 * part that decides that is the origin.
 */
describe("UrlToast", () => {
  const noop = () => {};

  it("shows the origin separately from the truncatable remainder", () => {
    render(
      <UrlToast
        url="https://github.com/login/device?code=ABCD-EFGH"
        onOpen={noop}
        onDismiss={noop}
      />,
    );
    expect(screen.getByTestId("url-toast-origin")).toHaveTextContent(
      "https://github.com",
    );
    expect(screen.getByTestId("url-toast-rest")).toHaveTextContent(
      "/login/device?code=ABCD-EFGH",
    );
  });

  it("keeps the origin intact when the path is long enough to push it out", () => {
    const url = `https://evil.tld/${"padding/".repeat(200)}end`;
    render(<UrlToast url={url} onOpen={noop} onDismiss={noop} />);
    // The registrable domain must be present in its own element, whole. A
    // single ellipsised line would render this and show only the padding.
    expect(screen.getByTestId("url-toast-origin")).toHaveTextContent(
      "https://evil.tld",
    );
  });

  it("exposes the whole URL as a tooltip", () => {
    const url = "https://example.com/a/b?c=d";
    render(<UrlToast url={url} onOpen={noop} onDismiss={noop} />);
    expect(screen.getByTestId("url-toast-url")).toHaveAttribute("title", url);
  });

  it("announces itself, so a replacement prompt is not silent", () => {
    render(
      <UrlToast url="https://example.com/" onOpen={noop} onDismiss={noop} />,
    );
    expect(screen.getByRole("status")).toBeInTheDocument();
  });

  it("opens only via the button, never on its own", () => {
    const onOpen = vi.fn();
    render(
      <UrlToast url="https://example.com/" onOpen={onOpen} onDismiss={noop} />,
    );
    expect(onOpen).not.toHaveBeenCalled();
    screen.getByRole("button", { name: "Open" }).click();
    expect(onOpen).toHaveBeenCalledTimes(1);
  });

  describe("Anthropic sign-in links", () => {
    // The callback listener a `claude login` is waiting on is *inside* the
    // container. Sending the user to their host browser completes the sign-in
    // and then posts the result where nothing is listening, and the terminal
    // hangs to its timeout — so for these, and only these, the container-side
    // browser leads.
    const SIGN_IN =
      "https://claude.ai/oauth/authorize?code=true&client_id=abc&response_type=code";

    function actions() {
      return screen
        .getAllByRole("button")
        .map((b) => b.textContent)
        .filter((t) => t === "Open" || t === "In container");
    }

    it("puts the container browser first", () => {
      render(
        <UrlToast
          url={SIGN_IN}
          onOpen={noop}
          onOpenInContainer={noop}
          onDismiss={noop}
        />,
      );
      expect(actions()).toEqual(["In container", "Open"]);
      expect(screen.getByTestId("url-toast-signin-hint")).toHaveTextContent(
        /callback listener is inside the container/i,
      );
    });

    it("keeps the host browser available as a fallback", () => {
      const onOpen = vi.fn();
      render(
        <UrlToast
          url={SIGN_IN}
          onOpen={onOpen}
          onOpenInContainer={noop}
          onDismiss={noop}
        />,
      );
      screen.getByRole("button", { name: "Open" }).click();
      expect(onOpen).toHaveBeenCalledTimes(1);
    });

    it("leaves an ordinary URL alone", () => {
      // A `gh auth login` device code, a docs page, a preview build — the host
      // browser is the right answer for all of them and stays the default.
      render(
        <UrlToast
          url="https://github.com/login/device?code=ABCD-EFGH"
          onOpen={noop}
          onOpenInContainer={noop}
          onDismiss={noop}
        />,
      );
      expect(actions()).toEqual(["Open", "In container"]);
      expect(screen.queryByTestId("url-toast-signin-hint")).not.toBeInTheDocument();
    });

    it("is not fooled by a lookalike host", () => {
      // `isAnthropicSignInUrl` uses the same allowlist the sign-in flow does,
      // so a URL that merely says "claude.ai" somewhere is not one.
      render(
        <UrlToast
          url="https://claude.ai.evil.tld/oauth/authorize?x=1"
          onOpen={noop}
          onOpenInContainer={noop}
          onDismiss={noop}
        />,
      );
      expect(actions()).toEqual(["Open", "In container"]);
    });
  });
});
