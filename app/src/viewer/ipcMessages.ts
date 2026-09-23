/**
 * The error strings the Rust side of the viewer produces and this side matches
 * on. This file is the one TypeScript copy; the Rust originals are
 *
 * - `CONFLICT_PREFIX`, `GONE_PREFIX`, `READ_ONLY_MESSAGE` in
 *   `src-tauri/src/file_viewer/write.rs` (`viewer_write_file` errors), and
 * - `NOT_RUNNING_PREFIX` in `src-tauri/src/commands/file_commands.rs`
 *   (`require_running` and the viewer's "no container" refusal).
 *
 * `write.rs`'s test `the_frontend_copies_of_the_ipc_messages_match` reads this
 * file and fails if any literal here drifts from its Rust original.
 */

/** A save refused because the file changed on disk since its base hash. */
export const CONFLICT_PREFIX = "conflict:";
/** A save refused because the file no longer exists. */
export const GONE_PREFIX = "gone:";
/** A save refused because the container user may not write the file. */
export const READ_ONLY_MESSAGE = "The file is read-only for the container user.";
/** Any command refused because the project's container is not running. */
export const NOT_RUNNING_PREFIX = "Start the project before";
