/**
 * The bytes that insert a newline in Claude Code's prompt without submitting
 * it: ESC then CR.
 *
 * These are the in-band bytes, not a guess — they are exactly what Claude
 * Code's own `/terminal-setup` writes into the VS Code, Cursor, Alacritty and
 * Zed keymaps, and `TerminalView`'s Shift+Enter handler has sent them since
 * that feature landed. **This must not be "simplified" to `\n`:** Claude Code
 * accepts `\n` too, but a shell would *run* the line, so the two session types
 * would quietly diverge.
 *
 * That last sentence is also why anything sending this must first check the
 * session is a Claude one. `bash -l`'s readline has no binding for `\e\r` and
 * answers with a bell.
 */
export const CLAUDE_SOFT_NEWLINE = "\x1b\r";

/**
 * Turn multi-line text into something that arrives in a Claude prompt as one
 * message.
 *
 * Sent as raw keystrokes, every `\n` submits, so an N-line note would arrive
 * as N truncated prompts. Deliberately appends no terminator: the text lands
 * in the prompt and the user presses Enter, which is what speech-to-text does
 * for the same reason — an unsent prompt is recoverable and a sent one is not.
 *
 * A **lone** `\r` is matched too, not only the one in a CRLF. It is a carriage
 * return: it submits in a Claude prompt and runs the line in a shell, which is
 * exactly the terminator this function promises never to append. A `<textarea>`
 * cannot produce one, but a note body is read back from a JSON file that can be
 * hand-edited or written by something else, so the guarantee has to hold for
 * whatever `load_in` returns rather than for whatever the editor can type.
 */
export function toClaudePayload(text: string): string {
  return text.replace(/\r\n|\r|\n/g, CLAUDE_SOFT_NEWLINE);
}
