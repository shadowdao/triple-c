import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent, act } from "@testing-library/react";
import PortMappingsEditor from "./PortMappingsEditor";
import type { PortMapping } from "../../lib/types";

const tcp = (host_port: number, container_port: number): PortMapping => ({
  host_port,
  container_port,
  protocol: "tcp",
});

function setup(initial: PortMapping[], extra: { tcpOnly?: boolean } = {}) {
  const onSave = vi.fn().mockResolvedValue(true);
  const view = render(
    <PortMappingsEditor portMappings={initial} disabled={false} onSave={onSave} {...extra} />,
  );
  const rerender = (next: PortMapping[]) =>
    view.rerender(
      <PortMappingsEditor portMappings={next} disabled={false} onSave={onSave} {...extra} />,
    );
  return { onSave, rerender };
}

const host = (n = 1) => screen.getByLabelText(`Host port ${n}`) as HTMLInputElement;
const container = (n = 1) => screen.getByLabelText(`Container port ${n}`) as HTMLInputElement;

function type(el: HTMLInputElement, value: string) {
  act(() => el.focus());
  fireEvent.change(el, { target: { value } });
}

describe("PortMappingsEditor", () => {
  it("keeps typed ports when the project refreshes mid-edit (the reported 0/0 save)", () => {
    // A stored half-row, as the old "+ Add" saved it, then a project refresh
    // (status event, save response, list reload) hands over a new-but-equal
    // array while the user is typing.
    const { onSave, rerender } = setup([tcp(0, 0)]);
    type(host(), "8080");
    rerender([tcp(0, 0)]);
    type(container(), "8080");
    rerender([tcp(0, 0)]);
    expect(host().value).toBe("8080");
    expect(container().value).toBe("8080");
    fireEvent.blur(container());
    expect(onSave).toHaveBeenLastCalledWith([tcp(8080, 8080)]);
  });

  it("keeps typed ports when its own earlier save's response arrives mid-edit", () => {
    const { onSave, rerender } = setup([]);
    fireEvent.click(screen.getByRole("button", { name: /add port mapping/i }));
    type(host(), "8080");
    fireEvent.blur(host());
    type(container(), "80");
    // A stale echo of an older stored value must not replace the edit in focus.
    rerender([]);
    type(container(), "8080");
    fireEvent.blur(container());
    expect(onSave).toHaveBeenLastCalledWith([tcp(8080, 8080)]);
    expect(container().value).toBe("8080");
  });

  it("does not save a newly added empty row", () => {
    const { onSave } = setup([]);
    fireEvent.click(screen.getByRole("button", { name: /add port mapping/i }));
    expect(onSave).not.toHaveBeenCalled();
    expect(host()).toHaveFocus();
    expect(screen.getByText(/fill in both ports/i)).toBeInTheDocument();
  });

  it("does not present an empty row as a filled-in mapping (no port-number placeholder)", () => {
    // The reported case: the empty row's "8080" placeholders read as 8080 → 8080.
    setup([]);
    fireEvent.click(screen.getByRole("button", { name: /add port mapping/i }));
    for (const el of [host(), container()]) {
      expect(el.value).toBe("");
      expect(el.placeholder).not.toMatch(/\d/);
    }
    expect(screen.getByText(/isn't saved or active/i)).toBeInTheDocument();
  });

  it("never saves a half-filled row, and saves it once both ports are in", () => {
    const { onSave } = setup([tcp(3000, 3000)]);
    fireEvent.click(screen.getByRole("button", { name: /add port mapping/i }));
    type(host(2), "8080");
    fireEvent.blur(host(2));
    expect(onSave).not.toHaveBeenCalled();
    expect(screen.getByText(/fill in both ports/i)).toBeInTheDocument();
    type(container(2), "9090");
    fireEvent.blur(container(2));
    expect(onSave).toHaveBeenLastCalledWith([tcp(3000, 3000), tcp(8080, 9090)]);
    expect(screen.queryByText(/fill in both ports/i)).not.toBeInTheDocument();
  });

  it("shows a stored incomplete row as incomplete", () => {
    setup([tcp(0, 0), tcp(8080, 8080)]);
    expect(screen.getAllByText(/fill in both ports/i)).toHaveLength(1);
    expect(host(1).value).toBe("");
  });

  it("refuses an out-of-range port without saving it", () => {
    const { onSave } = setup([]);
    fireEvent.click(screen.getByRole("button", { name: /add port mapping/i }));
    type(host(), "70000");
    type(container(), "80");
    fireEvent.blur(container());
    expect(onSave).not.toHaveBeenCalled();
    expect(screen.getByText(/1 and 65535/)).toBeInTheDocument();
  });

  it("takes a changed stored value once the editor is not being edited", () => {
    const { rerender } = setup([tcp(3000, 3000)]);
    rerender([tcp(4000, 4000)]);
    expect(host().value).toBe("4000");
  });

  it("removing a row saves the rest; the protocol saves at once (Docker keeps UDP)", () => {
    const { onSave } = setup([tcp(3000, 3000), tcp(4000, 4000)]);
    fireEvent.change(screen.getByLabelText("Protocol 2"), { target: { value: "udp" } });
    expect(onSave).toHaveBeenLastCalledWith([tcp(3000, 3000), { ...tcp(4000, 4000), protocol: "udp" }]);
    fireEvent.click(screen.getByRole("button", { name: "Remove port mapping 1" }));
    expect(onSave).toHaveBeenLastCalledWith([{ ...tcp(4000, 4000), protocol: "udp" }]);
  });

  it("does not save when nothing changed", () => {
    const { onSave } = setup([tcp(3000, 3000)]);
    act(() => host().focus());
    fireEvent.blur(host());
    expect(onSave).not.toHaveBeenCalled();
  });
});
