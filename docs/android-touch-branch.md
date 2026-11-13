# Android touch branch (fork)

This branch (`feature/touch`) carries this fork's Android and touch work on top
of upstream `main`. It exists so the work can be merged and re-merged without
re-deriving what is ours: the notes below say what differs, which files are
likely to conflict next time, what must survive a merge, and how to check the
result. For the upstream Android shell itself (build recipe, folder picker,
splash), see [android.md](android.md).

## Where the branch stands

- Merge base: upstream `main` at `1ea16933b` (v0.3.2), merged in `34966b81d`.
- The original fork branch `feature/android` (16 commits) still holds the work
  not ported here: the IME text dialog, tap snapping, and the render/pacing
  commits — see Known residuals.
- Upstream has moved on since: `3978fd185` is about 70 commits further, touching
  `handler.rs`, `dispatch.rs`, `input/state.rs`, `state.rs`, `frame.rs`,
  `shell_main_menu.rs` and `util/mod.rs` — every shared file this branch edits
  except `touch.rs`, `status_screen.rs`, `selection.rs`, `asset_import.rs` and
  `android/`. Expect conflicts in those shared files on the next merge.

Quick look at the difference:

```sh
git log --oneline main..feature/touch
git diff --stat main...feature/touch   # three dots: from the merge base
git show <commit>                      # the whole change, one commit at a time
```

## Commits on this branch

| Commit | What it does |
|---|---|
| `45133de5f` | Touch translator follows Rusted Warfare: one-finger battlefield drag pans, two fingers box-select, two-finger tap cancels, long still touch is the right click; `mouse.shp` hides while a finger drives the pointer. |
| `7d294a9e8` | Android adopts external app storage as the data folder, auto-writes `config.toml` for hand-copied `ra2/`, hides the system bars, and turns Back into Escape. |
| `34966b81d` | Merge of `main` at 0.3.2. |
| `74b1da4f0` | Touch tap on a unit the multi-selection already holds selects just that unit. |
| `d734fc2b6` | ZIP import beside the folder import (`app::game_import`, ZIP reader, Kotlin picker, JNI callback). |
| `8a9edc302` | The import button's chooser dialog is shown on the UI thread (JNI arrives on the game thread). Superseded in behaviour by `a6fc09d11`; kept in history. |
| `ede2bbd91` | `nativeOnZipPicked` joins the activity's `RegisterNatives` table (dynamic lookup for the app class fails on this device). |
| `a6fc09d11` | One import card: folder and ZIP buttons on the startup card itself, no second dialog; progress/outcome shown on the card. |
| `23c407e69` | Larger card buttons and labels for hand-held screens. |

New files: `src/app/game_import/{mod,zip,tests}.rs`, `src/app/input/touch_reselect_tests.rs`,
`src/util/zip_picker.rs`.

## Changes by area

### Touch input (share with upstream `touch.rs` carefully)

- `src/app/input/touch.rs` — replaced upstream's mapper wholesale with the
  Rusted Warfare gesture set. Gestures: tap = left click; one-finger drag that
  started over the battlefield = camera pan (grab style, `pan` output); a drag
  over the sidebar/menu keeps the left button held; two-finger drag = select box
  over the rectangle both contacts span (`band_anchor`); two-finger tap =
  cancel selection (`deselect`); long still touch = right click. Upstream will
  keep editing its own mapper; a merge of this file is a decision, not a
  three-way patch — take this branch's file unless upstream adopts the same
  gestures.
- `src/app/handler.rs` — the touch call site computes `over_battlefield` for
  the contact before translating, and applies `output.pan`, `output.deselect`
  and `output.band_anchor` (through `dispatch::reanchor_touch_box`). Also holds
  the Android Back→Escape rewrite.
- `src/sim/selection.rs` — `SelectionState::reanchor_drag` moves a live drag's
  fixed corner so the two-finger box follows both edges; release still resolves
  through `end_drag`.
- `src/app/input/dispatch.rs` — `reanchor_touch_box`; `touch_reselects_selected_unit`
  (touch-only guard that skips the native multi-select context route so a tap
  on a group member clears the selection to just it), with its decision half
  `touch_reselects_selected_unit_parts` tested in `touch_reselect_tests.rs`.
- `src/app/state.rs` — `AppState::software_cursor_drawn()` = software cursor
  allowed and no finger on the pointer.
- `src/app/input/state.rs` — `cursor_from_touch` starts true on Android.
- `src/app/frontend/shell_pass.rs`, `src/app/presentation/ui_overlays.rs` —
  both software-cursor paths gate on `software_cursor_drawn()`.

### Android shell

- `android/src/lib.rs` — data folder prefers `external_data_path()` and falls
  back to internal; `adopt_pushed_game_files()` writes `config.toml` when
  `ra2/ra2md.mix` exists and no config does; `zip_picker` hook and
  `nativeOnZipPicked` callback. **Invariant:** every native method the activity
  calls must be added to `register_folder_picker_native()`'s `RegisterNatives`
  table — dynamic symbol lookup for `org.vera20k.VeraActivity` fails on this
  device, and a missing entry kills the process with `UnsatisfiedLinkError`.
- `android/app/src/main/java/org/vera20k/VeraActivity.kt` — `dataDir()`
  (external when mounted), immersive system-bar hiding in `onCreate` /
  `onWindowFocusChanged`, `startFolderPicker()` and `startZipPicker()` opened on
  the UI thread via `runOnUiThread`, ZIP copy-to-cache → `nativeOnZipPicked`.
- `src/app/handler.rs` — `android_back_as_escape_key` rewrites `BrowserBack`
  to Escape before any consumer sees it.

### Game-file import

- `src/ui/asset_import.rs` — state only: `Copying`/`Cancelling`/`Done`/`Failed`,
  `status_note()` and `imported_line()`; `choose_folder()` / `choose_zip()`
  request the platform pickers. There is no dialog.
- `src/app/frontend/status_screen.rs` — the startup card's buttons: imports
  (folder, ZIP) + Quit when idle, Cancel while copying, Quit when done. `Buttons`
  now holds three slots; `CardLayout` sizes the buttons for touch and gives
  labels their own base size.
- `src/app/shell_main_menu.rs` — the import step and the imported path are
  appended to the card's text; the old dialog render is gone.
- `src/app/game_import/` — ZIP reader (`zip.rs`: stored/deflate only, refuses
  ZIP64, encryption, symlinks and escaping member paths, skips macOS metadata)
  and the unpack: into `<data>/ra2`, keep the shallowest folder holding
  `ra2md.mix` and `ra2.mix`, update `config.toml` through the config owner,
  report progress through `util::import_status`.
- `src/util/zip_picker.rs` — request hook for the platform ZIP picker.
- `src/i18n/vera20k.en.ini`, `vera20k.zh-CN.ini` — `VERA:ImportGameFolder`,
  `VERA:ImportGameZip`, `VERA:NoZipPicker`. Other languages fall back to English.

## Merge playbook

1. `git fetch origin && git switch feature/touch && git merge origin/main`.
2. Resolve by owner:
   - `src/app/input/touch.rs` — keep this branch's file unless upstream's
     mapper now covers the same gestures; if upstream changed its own, diff both
     and port upstream's non-gesture fixes (event dedupe, platform gates).
   - `handler.rs`, `dispatch.rs`, `input/state.rs`, `state.rs`, `frame.rs`,
     `shell_main_menu.rs` — take upstream's side, then re-apply this branch's
     additions from the commit that introduced them (`git show 45133de5f`,
     `7d294a9e8`, `74b1da4f0`, `a6fc09d11`): the touch call-site block, the
     Back→Escape rewrite, `software_cursor_drawn`/`cursor_from_touch`, the
     import text appended to the card, the `reanchor_touch_box` helper.
   - `android/` and `VeraActivity.kt` — upstream own the shell; re-apply
     `dataDir()`, the immersive overrides, the two picker entry points and
     `nativeOnZipPicked`, and add the callback to the `RegisterNatives` table.
   - `status_screen.rs`, `asset_import.rs`, `game_import/`, `zip_picker.rs`,
     `selection.rs`, `touch_reselect_tests.rs` — this branch owns them.
3. Keep the two buttons plus Quit on the card: there is no separate dialog, so
   `Buttons` must stay three slots wide and `for_shell_error` must keep the
   folder/ZIP/Quit and Cancel/Done states.

## Validation after a merge

- `python -m tools.cargo_run -- check -p vera20k --lib --bins`
- `python -m tools.cargo_run -- test -p vera20k --lib -- app::input::touch::`
- `python -m tools.cargo_run -- test -p vera20k --lib -- ui::asset_import`
- `python -m tools.cargo_run -- test -p vera20k --lib -- game_import`
- `python -m tools.cargo_run -- test -p vera20k-core --lib -- sim::selection::`
- `python -m tools.cargo_run -- test -p vera20k --lib -- touch_reselect_tests`
- `python -m tools.cargo_run -- check -p vera20k-android --target aarch64-linux-android`
- Build the APK (`cargo ndk`/`cargo build --target aarch64-linux-android` then
  `./android/gradlew -p android :app:assembleDebug`) and on device check:
  system bars hidden, Back opens the menu, folder and ZIP imports from the
  missing-files card, two-finger box and one-finger pan in a match, tap on a
  group member selects just it.

## Known residuals

- **No on-device text input.** The fork's system `EditText` dialog for the LAN
  lobby name/chat (commit `5b3c5f9c5` on `feature/android`) is not ported; the
  upstream doc still lists the missing IME as an open item.
- **No tap snapping.** `ae7e7f89a` on `feature/android` snaps a touch click to
  the unit it missed; not ported. If taps near a unit miss on device, port that.
- **Render/pacing work not ported.** `79a729435` (Back key — the key part is
  ported — catch-up pacing, terrain palette decode table), `3477bb56f` (poll the
  device once per frame) and `8043ebd24` (Studio jniLibs dir) touch upstream
  render paths that moved 500+ commits; evaluate them separately.
- **No auto restart after an import.** The fork's ZIP import restarted the
  activity; this branch follows upstream's "restart VERA20K to play" instead,
  and the Done state's only button is Quit.
- **Import progress redraws with the card.** `util::import_status` events are
  drained when the card draws; a fast copy can skip visible updates (upstream's
  folder import has the same limitation).
