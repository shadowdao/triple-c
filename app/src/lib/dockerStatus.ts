import type { DockerStatus } from "./types";

/** Short label for status dots and checklists. */
export function dockerFailLabel(status: DockerStatus | null): string {
  switch (status?.state) {
    case "not_running":
      return "Docker not running";
    case "no_permission":
      return "Docker access denied";
    case "not_installed":
      return "Docker not installed";
    default:
      return "Docker unavailable";
  }
}
