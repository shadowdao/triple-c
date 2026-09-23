import { imageMimeFor, looksBinary, TEXT_PREVIEW_LIMIT } from "../components/projects/home/filePreview";
import type { ViewerFile } from "../lib/types";

export type ViewerKind = "text" | "image" | "binary";
export interface Editability { kind: ViewerKind; editable: boolean; reason: string | null }

const MIB = TEXT_PREVIEW_LIMIT / (1024 * 1024);

export function classifyViewerFile(path: string, file: ViewerFile, bytes: Uint8Array): Editability {
  if (imageMimeFor(path)) return { kind: "image", editable: false, reason: "Images are shown, not edited." };
  if (looksBinary(bytes)) return { kind: "binary", editable: false, reason: "This file is not text." };
  if (file.truncated) return { kind: "text", editable: false, reason: `Files over ${MIB} MiB are read-only.` };
  if (!file.editable) return { kind: "text", editable: false, reason: file.readonly_reason ?? "This location is read-only." };
  return { kind: "text", editable: true, reason: null };
}
