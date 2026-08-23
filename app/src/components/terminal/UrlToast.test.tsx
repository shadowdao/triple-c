import { describe, it, expect, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import UrlToast, { URL_TOAST_PRIMARY_SELECTOR } from "./UrlToast";

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

  describe("keyboard", () => {
    // This toast is the only route to completing a sign-in started in a
    // terminal, and xterm's helper textarea swallows Tab — so without these it
    // is unreachable for a keyboard-only user.
    const SIGN_IN =
      "https://claude.ai/oauth/authorize?code=true&client_id=abc&response_type=code";

    it("does not take focus from the live terminal when it appears", () => {
      // Deliberate. The user may be mid-command, and the default action opens a
      // URL the *container* chose — a focused button is one stray Enter away
      // from doing it. The shortcut hint below is what makes that affordable.
      render(
        <UrlToast url="https://example.com/" onOpen={noop} onDismiss={noop} />,
      );
      expect(document.activeElement).toBe(document.body);
    });

    it("says how to reach it, since nothing announces a shortcut by itself", () => {
      render(
        <UrlToast url="https://example.com/" onOpen={noop} onDismiss={noop} />,
      );
      expect(screen.getByTestId("url-toast-shortcut")).toHaveTextContent(
        "Ctrl+Shift+O",
      );
    });

    it("marks the default action so the shortcut has somewhere to land", () => {
      // Which button that is depends on the URL, so the marker moves with the
      // decision rather than the owner having to repeat it.
      const { rerender } = render(
        <UrlToast
          url="https://github.com/login/device?code=ABCD"
          onOpen={noop}
          onOpenInContainer={noop}
          onDismiss={noop}
        />,
      );
      expect(
        document.querySelector(URL_TOAST_PRIMARY_SELECTOR),
      ).toHaveTextContent("Open");

      rerender(
        <UrlToast
          url={SIGN_IN}
          onOpen={noop}
          onOpenInContainer={noop}
          onDismiss={noop}
        />,
      );
      expect(
        document.querySelector(URL_TOAST_PRIMARY_SELECTOR),
      ).toHaveTextContent("In container");
    });

    it("dismisses on Escape from anywhere inside it", () => {
      const onDismiss = vi.fn();
      render(
        <UrlToast
          url="https://example.com/"
          onOpen={noop}
          onDismiss={onDismiss}
        />,
      );
      fireEvent.keyDown(screen.getByRole("button", { name: "Open" }), {
        key: "Escape",
      });
      expect(onDismiss).toHaveBeenCalledTimes(1);
    });

    it("does not answer Escape pressed outside it", () => {
      // Escape belongs to whatever is running in the terminal — vim, above all.
      // A document-level binding would break it for everyone who never looked
      // at this toast.
      const onDismiss = vi.fn();
      render(
        <UrlToast
          url="https://example.com/"
          onOpen={noop}
          onDismiss={onDismiss}
        />,
      );
      fireEvent.keyDown(document.body, { key: "Escape" });
      expect(onDismiss).not.toHaveBeenCalled();
    });

    it("gives every action a real button, so Tab reaches all three", () => {
      render(
        <UrlToast
          url={SIGN_IN}
          onOpen={noop}
          onOpenInContainer={noop}
          onDismiss={noop}
        />,
      );
      const names = screen
        .getAllByRole("button")
        .map((b) => b.getAttribute("aria-label") ?? b.textContent);
      expect(names).toEqual(["In container", "Open", "Dismiss"]);
      // Nothing is taken out of the tab order.
      for (const b of screen.getAllByRole("button")) {
        expect(b).not.toHaveAttribute("tabindex", "-1");
      }
    });
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
