import {
  useId,
  type KeyboardEventHandler,
  type MouseEventHandler,
  type ReactNode,
} from "react";

/** Keys a native `<button>` turns into a click. */
const ACTIVATION_KEYS = new Set([" ", "Spacebar", "Enter"]);

export interface UnavailableControlProps {
  "aria-disabled"?: true;
  "aria-describedby"?: string;
  onClick?: MouseEventHandler<HTMLButtonElement>;
  onKeyDown?: KeyboardEventHandler<HTMLButtonElement>;
}

export interface UnavailableControl {
  /** Spread onto the control. Carries the guarded handlers. */
  controlProps: UnavailableControlProps;
  /**
   * Render as a *sibling* of the control — inside it the reason would be
   * appended to the accessible name instead of the description.
   */
  reasonNode: ReactNode;
}

/**
 * Makes a control unavailable without hiding it from assistive technology.
 *
 * `disabled` takes an element out of the tab order *and* out of the
 * accessibility tree, so the `title` explaining why it cannot be used is
 * announced to nobody and shown only to a sighted user with a mouse. That is
 * backwards: the people who most need the reason are the ones who never get
 * it. `aria-disabled` keeps the control focusable and announced, and
 * `aria-describedby` hands over the reason.
 *
 * The catch is that `aria-disabled` is advisory — it does not block clicks or
 * Enter/Space the way `disabled` does. This hook therefore returns the guards
 * along with the attributes, so a call site cannot take the announcement
 * without the guard. Handlers that a form can reach without going through the
 * control (Enter inside a text field submits the form) still have to guard
 * themselves.
 */
export function useUnavailable({
  unavailable,
  reason,
  onClick,
  onKeyDown,
}: {
  unavailable: boolean;
  reason: string;
  onClick?: MouseEventHandler<HTMLButtonElement>;
  onKeyDown?: KeyboardEventHandler<HTMLButtonElement>;
}): UnavailableControl {
  const reasonId = `${useId()}unavailable`;

  if (!unavailable) {
    return { controlProps: { onClick, onKeyDown }, reasonNode: null };
  }

  return {
    controlProps: {
      "aria-disabled": true,
      "aria-describedby": reasonId,
      onClick: (e) => {
        e.preventDefault();
        e.stopPropagation();
      },
      onKeyDown: (e) => {
        if (!ACTIVATION_KEYS.has(e.key)) {
          onKeyDown?.(e);
          return;
        }
        // Suppress the default action before it can become a click, submit a
        // form, or scroll the page.
        e.preventDefault();
        e.stopPropagation();
      },
    },
    reasonNode: (
      <span id={reasonId} className="sr-only">
        {reason}
      </span>
    ),
  };
}
