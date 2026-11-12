//! Importing the retail game files from a ZIP the player picks.
//!
//! The startup card's import dialog can hand the platform a file chooser for a
//! ZIP of the player's own Yuri's Revenge copy; Android copies the chosen
//! document into its cache and reports the filesystem path here, because a
//! `content://` URI has no path the extractor can open. The archive is unpacked
//! into the app's game folder, `config.toml` is pointed at the folder that holds
//! `ra2md.mix` and `ra2.mix`, and the dialog asks for a restart through the
//! ordinary import events (`util::import_status`).
//!
//! The extraction, the ZIP reader and the config write are platform
//! independent: the platform only supplies the archive's path.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::util::import_status::{self, ImportEvent};

mod zip;

#[cfg(test)]
mod tests;

/// Why an import stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Failure {
    /// The archive could not be read at all.
    Unreadable(String),
    /// The archive holds no folder with the game's two master archives.
    NoGameFiles,
    /// Writing the unpacked files failed.
    Write(String),
    /// The files are there but config.toml could not name their folder.
    Config(String),
    /// The player cancelled while the archive was being unpacked.
    Cancelled,
}

impl Failure {
    /// The sentence the import card shows below its localised heading.
    fn reason(&self) -> String {
        match self {
            Self::Unreadable(detail) => format!("The ZIP file could not be read: {detail}"),
            Self::NoGameFiles => {
                "The ZIP has no ra2md.mix and ra2.mix. Pick the complete game archive.".to_owned()
            }
            Self::Write(detail) => format!("Writing the game files failed: {detail}"),
            Self::Config(detail) => {
                format!(
                    "The game folder was unpacked, but config.toml could not be updated: {detail}"
                )
            }
            Self::Cancelled => "The import was cancelled.".to_owned(),
        }
    }
}

/// Bytes between two progress events: a fast storage device must not flood the
/// frame loop with one event per member.
const PROGRESS_STEP_BYTES: u64 = 16 * 1024 * 1024;

/// Set while an unpack is running; a second start waits for it to end.
static RUNNING: AtomicBool = AtomicBool::new(false);
/// Set by [`cancel`] to stop the running unpack at its next member.
static CANCEL: AtomicBool = AtomicBool::new(false);
/// Bytes unpacked so far, and the archive's total, for the progress line.
static UNPACKED: AtomicU64 = AtomicU64::new(0);
static ARCHIVE_BYTES: AtomicU64 = AtomicU64::new(0);

/// Unpack `zip` on a thread of its own, reporting through
/// `util::import_status` so the dialog advances on the next drawn frame.
///
/// A second call while one unpack runs is ignored: the dialog only offers one
/// source at a time.
pub fn start(zip: PathBuf) {
    if RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    CANCEL.store(false, Ordering::SeqCst);
    let Some(data_root) = data_root() else {
        RUNNING.store(false, Ordering::SeqCst);
        import_status::note(ImportEvent::Failed(
            "no writable app folder to unpack into".to_owned(),
        ));
        return;
    };
    import_status::note(ImportEvent::Started);
    let spawned = std::thread::Builder::new()
        .name("vera20k-zip-import".to_owned())
        .spawn(move || {
            let result = import_into(&zip, &data_root, &mut |written| {
                import_status::note(ImportEvent::Progress(written));
                !CANCEL.load(Ordering::SeqCst)
            });
            // The platform copied the archive into its cache; only the
            // unpacked files stay.
            let _ = std::fs::remove_file(&zip);
            match result {
                Ok(game) => import_status::note(ImportEvent::Done(game.display().to_string())),
                Err(Failure::Cancelled) => import_status::note(ImportEvent::Cancelled),
                Err(failure) => import_status::note(ImportEvent::Failed(failure.reason())),
            }
            RUNNING.store(false, Ordering::SeqCst);
        });
    if let Err(error) = spawned {
        RUNNING.store(false, Ordering::SeqCst);
        import_status::note(ImportEvent::Failed(format!(
            "cannot start the import thread: {error}"
        )));
    }
}

/// Stop the running unpack. `true` means one was running and will report its
/// own cancel event when it stops.
pub fn cancel() -> bool {
    let running = RUNNING.load(Ordering::SeqCst);
    if running {
        CANCEL.store(true, Ordering::SeqCst);
    }
    running
}

/// Unpacked bytes and the archive's total, for the card's progress line.
pub(crate) fn progress_bytes() -> (u64, u64) {
    (
        UNPACKED.load(Ordering::Relaxed),
        ARCHIVE_BYTES.load(Ordering::Relaxed),
    )
}

/// The app's writable folder: where the ZIP's files land and where
/// `config.toml` names them.
fn data_root() -> Option<PathBuf> {
    crate::util::platform_dir::get()
        .map(Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok())
}

/// Unpack `zip` into `data_root/ra2`, then point `data_root/config.toml` at the
/// folder that holds the game. Returns that folder.
///
/// `progress` receives the bytes written so far and returns whether to keep
/// going; `false` stops the unpack as [`Failure::Cancelled`].
pub(crate) fn import_into(
    zip: &Path,
    data_root: &Path,
    progress: &mut dyn FnMut(u64) -> bool,
) -> Result<PathBuf, Failure> {
    let game_folder = data_root.join("ra2");
    std::fs::create_dir_all(&game_folder)
        .map_err(|error| Failure::Write(format!("create {}: {error}", game_folder.display())))?;
    let mut archive = zip::Archive::open(zip).map_err(Failure::Unreadable)?;
    ARCHIVE_BYTES.store(archive.total_bytes(), Ordering::Relaxed);
    UNPACKED.store(0, Ordering::Relaxed);
    unpack(&mut archive, &game_folder, progress)?;
    let game_root = find_game_root(&game_folder).ok_or(Failure::NoGameFiles)?;
    write_config(data_root, &game_root)?;
    Ok(game_root)
}

/// Write every member below `root`, stopping when `progress` says so.
fn unpack(
    archive: &mut zip::Archive,
    root: &Path,
    progress: &mut dyn FnMut(u64) -> bool,
) -> Result<(), Failure> {
    let entries = archive.entries().to_vec();
    let mut written = 0u64;
    let mut reported = 0u64;
    for entry in &entries {
        if entry.is_dir() || entry.is_symlink() {
            continue;
        }
        let Some(relative) = entry.relative_path() else {
            continue;
        };
        let data = archive.read(entry).map_err(Failure::Write)?;
        let destination = root.join(&relative);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| Failure::Write(format!("create {}: {error}", parent.display())))?;
        }
        // Replace a member that is already there: a re-import overwrites the
        // previous copy of the game rather than mixing two installations.
        std::fs::write(&destination, &data)
            .map_err(|error| Failure::Write(format!("write {}: {error}", destination.display())))?;
        written += data.len() as u64;
        UNPACKED.store(written, Ordering::Relaxed);
        if written - reported >= PROGRESS_STEP_BYTES {
            reported = written;
            if !progress(written) {
                return Err(Failure::Cancelled);
            }
        }
    }
    if !progress(written) {
        return Err(Failure::Cancelled);
    }
    Ok(())
}

/// The shallowest folder below `root` (itself included) that holds both master
/// archives, so a ZIP that keeps its game in a top folder works as well as one
/// that unpacks straight into the game folder.
pub(crate) fn find_game_root(root: &Path) -> Option<PathBuf> {
    let holds_game =
        |folder: &Path| folder.join("ra2md.mix").is_file() && folder.join("ra2.mix").is_file();
    if holds_game(root) {
        return Some(root.to_path_buf());
    }
    // Breadth first, so the shallowest folder wins.
    let mut pending = std::collections::VecDeque::from([(root.to_path_buf(), 0u32)]);
    while let Some((folder, depth)) = pending.pop_front() {
        if depth >= 3 {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let child = entry.path();
            if holds_game(&child) {
                return Some(child);
            }
            pending.push_back((child, depth + 1));
        }
    }
    None
}

/// Point config.toml at `game_root`, keeping every other setting.
fn write_config(data_root: &Path, game_root: &Path) -> Result<(), Failure> {
    let path = data_root.join("config.toml");
    let existing = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(Failure::Config(format!("read {}: {error}", path.display())));
        }
    };
    let Some(game_dir) = game_root.to_str() else {
        return Err(Failure::Config(
            "the game folder path is not valid UTF-8".to_owned(),
        ));
    };
    let text = crate::util::config::set_paths_key(&existing, "ra2_dir", game_dir);
    std::fs::write(&path, text).map_err(|error| Failure::Config(format!("write: {error}")))
}
