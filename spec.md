This is a simple komorebi window manager status indicator in a system tray icon, written in Rust.

There is a build and distribution setup in the project for
reproducible app builds.

It shows a system tray icon that is a grid of 9 squares (numbered from 0 to 8 from left-top corner, 0..2 in the first row, 3..5 in the second, and 6..8 in the last).

Each element in the grid, numbered as `i`, represents a komorebi workspace numbered `i`.
The indicators available are the following:
* `i`-th workspace is focused - highlighted with a bright blue color;
* `i`-th workspace has a full-screen app - the square has a yellow border (might be displayed together with the focused state);
* `i`-th workspace is non-empty - the square is filled with gray;
* `i`-th workspace is empty - the square is not filled, i.e. has a transparent background.

The app gets the status from komorebi window manager by talking to komorebi's IPC directly, in-process, through the upstream Rust crates published by the komorebi project (see the **komorebi integration** requirement below). It subscribes to komorebi's event stream and parses the `State` payload that komorebi pushes on every notification (and that the app can also query on demand for initial seeding / reconnect). Workspaces are available per-monitor, under `monitors` / `elements[]` / `workspaces[]` (and each item in the array has `containers[]` that can be empty or non-empty). If there are fewer workspaces than the grid can display, assume that the others are empty. The numbers to match the workspaces with the squares are implicit in the status, they are not in the output, the order sets them.

When there is more than one monitor, the app should show one tray icon per monitor, with the corresponding monitor's workspaces content in each icon. Every icon — whether there is one monitor or several — is decorated with a small outer border: the icon for the currently focused monitor uses the focused (blue) color so the active monitor can be told apart at a glance, and the icons for any inactive monitors use the non-empty (gray) color. With a single monitor the border is always drawn in the focused (blue) color, since that monitor is by definition the active one. This keeps the icon footprint uniform across single- and multi-monitor setups and makes the active highlight read as a state change rather than a size change.

The app has an autostart feature that can be enabled or disabled in the 
right-click menu on the tray icon. Each tray icon should show the same right-click menu.

### komorebi integration

The app must integrate with komorebi by depending on komorebi's own Rust crates (the `komorebi-client` crate, which transitively pulls in the rest of the upstream `komorebi` workspace) rather than by shelling out to `komorebic.exe`. The rationale is twofold: (a) wire-format and schema changes in komorebi (the `SocketMessage` enum, the `State`/`Notification` types, the set of override events, the socket path layout, etc.) are picked up by a `cargo update` instead of by silently breaking JSON parsing or CLI flag handling, and (b) the integration runs entirely in-process, with no per-event subprocess spawn, no console-window flashes, and no dependency on `komorebic.exe` being on `PATH` at run time.

Concretely:
* The dependency on the komorebi crates must be **pinned to an upstream release tag** (e.g. `tag = "v0.1.41"` in `Cargo.toml`), not to a floating branch or a bare git revision, so that builds are reproducible and an intentional human action is required to track a new komorebi release.
* All komorebi IPC — subscribing for notifications, querying the full `State`, and any control messages the app needs to send — must go through the upstream types and helpers (`SocketMessage`, `subscribe`, `send_query`, `send_message`, …). The app must not parse JSON emitted by `komorebic.exe` or reimplement komorebi's socket-path / framing conventions by hand.
* `komorebic.exe` is not required to be installed or on `PATH` at run time. (The komorebi service itself — i.e. the `komorebi.exe` daemon — is of course required; it owns the AF_UNIX control socket the app connects to.)

### Surviving komorebi restarts

The app must survive komorebi restarts (`komorebic stop` followed by `komorebic start`, an upgrade-in-place, or a crash-and-respawn) and resume live updates without user intervention and without the user having to restart the tray. Because komorebi keeps its subscriber registry only in process memory, our subscription is forgotten the moment the komorebi daemon exits — even if our listening socket is still bound on disk — so the app cannot rely on a passive listener to recover on its own.

The app must therefore:
* **Detect** that komorebi has gone down and come back up, on a timescale that feels responsive to the user (single-digit seconds), without busy-looping and without spamming komorebi or its other subscribers with traffic on every probe. In particular, a liveness check must not cause komorebi to broadcast state to *other* subscribers (yasb, komokana, …) — it must only touch komorebi's own connection-accept path.
* **Re-register** the subscription with komorebi as soon as komorebi is reachable again, using a stable subscriber identity so the re-registration is idempotent (komorebi keys subscribers by name in a `HashMap`) and komorebi's registry doesn't grow on every reconnect.
* **Re-seed the UI** after each reconnect so the tray reflects reality before the first post-restart event arrives, ideally by piggy-backing on whatever broadcast komorebi already emits as a side effect of (re-)registration rather than issuing a separate explicit state query.
* **Not** rely on interrupting a blocking `accept()` on the subscription socket (there is no portable way to do that on Windows). Liveness detection must run independently of the accept loop.
* **Back off gracefully** on hard errors (subscribe/send/accept failures) so a flapping komorebi cannot turn into a reconnect storm, and reset that back-off once a session has been healthy for long enough.

### Behavioral defaults

These are the visual and behavioral choices that should not be left to the
implementor's discretion, because plausible alternatives would produce a
materially different product:

* **Colors (RGBA hex)**: focused = `#2E9BFFFF`, full-screen border = `#FFD500FF`, non-empty = `#808080FF`, empty = fully transparent. The active-monitor outer border uses the focused color; the inactive-monitor outer border uses the non-empty color.
* **Icon resolution**: each tray icon is rendered as a 32×32 RGBA bitmap; Windows scales it for hi-DPI displays.
* **Full-screen border placement**: 2 px yellow border drawn *inside* the cell (it overlays the existing fill and does not extend into neighbouring cells).
* **Monitor outer border placement**: 1 px outer border around the whole 3×3 grid, drawn on every icon. It uses the focused color for the active monitor and the non-empty color for inactive monitors. On a single-monitor setup the only icon gets the focused (blue) border.
* **"No such workspace" vs. empty workspace**: rendered identically (transparent cell). The icon does not visually distinguish "workspace exists but has no windows" from "this workspace index is beyond what komorebi reports".
* **More than 9 workspaces**: cells 0..8 are rendered as usual; workspaces with index ≥ 9 are silently ignored.
* **Single-instance scope is per-user**: the singleton guard must be scoped to the current Windows user session (e.g. `Local\…` mutex), so two different users on the same machine (RDP, fast user switching) can each run their own instance.
* **Behavior when komorebi is unreachable at startup**: the app keeps running and retries with the same backoff used for reconnect; it does *not* exit on first failure. This way, installing or starting komorebi later self-heals without restarting the app.
* **Left/double-click on tray icons**: no-ops. Only right-click shows the menu.
* **`Quit` is global**: clicking `Quit` on any tray icon's menu terminates the whole process and removes every tray icon, not just the icon that was clicked.
* **Menu state refresh**: the `Enable autostart` checkmark is refreshed from the registry every time the menu is about to be shown, so external changes (e.g. user editing the registry by hand) are reflected without restarting the app.
* **Autostart target path**: the registry entry under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` stores a quoted absolute path to `current_exe()`. The recommended deployment is to install the binary to a stable per-user location (e.g. `%LOCALAPPDATA%\Programs\komorebi-tray-grid\komorebi-tray-grid.exe`) before enabling autostart, so moving or rebuilding the binary does not silently break it.