import { createContext, useContext, type ReactNode } from "react";

/**
 * "Is the pane I belong to the one on screen?"
 *
 * The main area keeps every tab *mounted* and hides the inactive ones with a
 * `hidden` class, so their state survives a tab switch. A `ui/Modal` opened
 * inside one of those panes does not go quiet when its pane does: it portals
 * to `document.body`, where an ancestor's `display:none` cannot reach it. So a
 * dialog opened in project A stayed painted over project B after a tab switch,
 * kept its focus trap and its Escape binding, and — because it is a blocking
 * overlay — refused every native file drop in the window.
 *
 * `App` publishes the answer around each pane it mounts — it is what decides
 * which one is on screen — and `Modal` reads it. Nothing else needs to:
 * dialogs are the only thing in the app that escapes its pane's subtree.
 *
 * Default `true`, so a dialog with no pane above it — host settings, the
 * Docker install prompt — behaves exactly as it always has.
 */
const PaneVisibilityContext = createContext(true);

export function PaneVisibilityProvider({
  visible,
  children,
}: {
  visible: boolean;
  children: ReactNode;
}) {
  return (
    <PaneVisibilityContext.Provider value={visible}>
      {children}
    </PaneVisibilityContext.Provider>
  );
}

/** True unless an ancestor pane says it is currently hidden. */
export function usePaneVisible(): boolean {
  return useContext(PaneVisibilityContext);
}
