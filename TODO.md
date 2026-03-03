# TODO / Future Improvements

## In-App Auto-Update via `tauri-plugin-updater`

**Priority:** High
**Status:** Planned

Currently the app detects available updates via the Gitea API (`check_for_updates` command) but cannot apply them. Users must manually download and install the new version. On macOS and Linux this is a poor experience compared to Windows (where NSIS handles upgrades cleanly).

### Recommended approach: `tauri-plugin-updater`

Full in-app auto-update: detects, downloads, verifies, and applies updates seamlessly on all platforms. The user clicks "Update" and the app restarts with the new version.

### Requirements

1. **Generate a Tauri update signing key pair** (this is Tauri's own Ed25519 key, not OS code signing):
   ```bash
   npx @tauri-apps/cli signer generate -w ~/.tauri/triple-c.key
   ```
   Set `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` in CI.

2. **Add `tauri-plugin-updater`** to Rust and JS dependencies.

3. **Create an update endpoint** that returns Tauri's expected JSON format:
   ```json
   {
     "version": "v0.1.100",
     "notes": "Changelog here",
     "pub_date": "2026-03-01T00:00:00Z",
     "platforms": {
       "darwin-x86_64": { "signature": "...", "url": "https://..." },
       "darwin-aarch64": { "signature": "...", "url": "https://..." },
       "linux-x86_64": { "signature": "...", "url": "https://..." },
       "windows-x86_64": { "signature": "...", "url": "https://..." }
     }
   }
   ```
   This could be a static JSON file uploaded alongside release assets, or a small API that reads from Gitea releases and reformats.

4. **Configure the updater** in `tauri.conf.json`:
   ```json
   "plugins": {
     "updater": {
       "endpoints": ["https://repo.anhonesthost.net/...update-endpoint..."],
       "pubkey": "<public key from step 1>"
     }
   }
   ```

5. **Add frontend UI** for the update prompt (replace or enhance the existing update check flow).

6. **Update CI pipeline** to:
   - Sign bundles with the Tauri key during build
   - Upload `.sig` files alongside installers
   - Generate/upload the update endpoint JSON

### References
- https://v2.tauri.app/plugin/updater/
- Existing update check code: `app/src-tauri/src/commands/update_commands.rs`
- Existing models: `app/src-tauri/src/models/update_info.rs`
