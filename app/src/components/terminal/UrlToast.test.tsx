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
});
