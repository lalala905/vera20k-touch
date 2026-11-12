//! Progress and outcome of an asset import, shown on the startup card.
//!
//! The startup-failure card carries the import buttons itself — one for a
//! folder, one for a ZIP — so there is no second dialog: pressing a button
//! opens the platform's picker directly (`util::folder_picker`,
//! `util::zip_picker`), and this type only owns what the card says while the
//! copy or unpack runs and how it ended. The platform reports through
//! `util::import_status`, and the card drains those events on the frame it
//! draws.
//!
//! Its words come from VERA's language files (`crate::i18n`): the card shows
//! before the game's string table can load.

/// Whether this build offers the import buttons.
///
/// Android only: the desktop builds keep the startup card as it was, because a
/// packaged desktop launch already has a native folder chooser.
pub(crate) const AVAILABLE: bool = cfg!(target_os = "android");

/// Where an import stands.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) enum ImportStep {
    /// No import has run, or the card was reset after one.
    #[default]
    Closed,
    /// Copying the installation into the app's own folder.
    Copying { done: u64, total: Option<u64> },
    /// Waiting for the copying worker to stop and remove its partial files.
    Cancelling,
    /// Copied; the engine needs a restart to read the new folder.
    Done,
    /// The last attempt failed; the reason is what the card shows.
    Failed(String),
}

/// The import's state on the card.
#[derive(Debug, Default)]
pub(crate) struct AssetImport {
    step: ImportStep,
    /// Where the finished copy landed. The running process still holds the
    /// config it read at start-up, so this is the only way the card can name the
    /// folder the next launch will use.
    imported: Option<String>,
}

impl AssetImport {
    pub(crate) fn step(&self) -> &ImportStep {
        &self.step
    }

    /// An import is running or waiting for its worker to stop; the card shows
    /// the progress and a Cancel button instead of the import buttons.
    pub(crate) fn is_busy(&self) -> bool {
        matches!(
            self.step,
            ImportStep::Copying { .. } | ImportStep::Cancelling
        )
    }

    /// Back to the plain card, leaving the error text underneath untouched.
    pub(crate) fn close(&mut self) {
        self.step = ImportStep::Closed;
        self.imported = None;
    }

    /// The platform handed a copy job its size (unknown for some sources,
    /// where `content://` reports no length).
    pub(crate) fn start_copy(&mut self, total: Option<u64>) {
        self.step = ImportStep::Copying { done: 0, total };
    }

    pub(crate) fn cancel_copy(&mut self) {
        self.step = ImportStep::Cancelling;
    }

    /// Bytes copied so far, so the card can show how far along it is.
    pub(crate) fn advance_copy(&mut self, done: u64) {
        if let ImportStep::Copying { total, .. } = self.step {
            self.step = ImportStep::Copying { done, total };
        }
    }

    /// The copy finished, at [path]: the card names that folder from here on,
    /// because the running process still holds the config it read at start-up.
    pub(crate) fn finish_copy(&mut self, path: impl Into<String>) {
        self.imported = Some(path.into());
        self.step = ImportStep::Done;
    }

    pub(crate) fn fail(&mut self, why: impl Into<String>) {
        self.step = ImportStep::Failed(why.into());
    }

    /// The step's sentence for the card, or nothing while no import has run.
    pub(crate) fn status_note(&self) -> Option<String> {
        use crate::i18n::{tr, tr_fill};
        const MB: u64 = 1024 * 1024;
        Some(match &self.step {
            ImportStep::Closed => return None,
            ImportStep::Copying {
                done,
                total: Some(total),
            } => tr_fill(
                "VERA:ImportCopyingOf",
                &[("done", &(done / MB)), ("total", &(total / MB))],
            ),
            ImportStep::Copying { done, total: None } => {
                tr_fill("VERA:ImportCopying", &[("done", &(done / MB))])
            }
            ImportStep::Cancelling => tr("VERA:ImportCancelling"),
            ImportStep::Done => tr("VERA:ImportCopied"),
            ImportStep::Failed(why) => tr_fill("VERA:ImportFailed", &[("reason", why)]),
        })
    }

    /// The line naming the copy once one exists, or nothing before that.
    pub(crate) fn imported_line(&self) -> Option<String> {
        self.imported
            .as_deref()
            .map(|path| crate::i18n::tr_fill("VERA:ImportGameFilesAt", &[("path", &path)]))
    }
}

/// Ask the platform for the folder holding the retail installation.
///
/// `false` means this build has no picker; `true` means the picker opened and
/// the platform will deliver the choice through `folder_picker` later.
pub(crate) fn choose_folder() -> bool {
    if !crate::util::folder_picker::request() {
        log::info!("asset import: this build has no folder picker wired yet");
        return false;
    }
    true
}

/// Ask the platform for a ZIP of the retail installation.
///
/// `false` means this build has no picker; `true` means the platform opened it
/// and will hand the archive to `app::game_import` when the player picks one.
pub(crate) fn choose_zip() -> bool {
    if !crate::util::zip_picker::request() {
        log::info!("asset import: this build has no ZIP picker wired yet");
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_card_says_nothing_until_an_import_starts() {
        let import = AssetImport::default();
        assert!(!import.is_busy());
        assert_eq!(import.status_note(), None);
        assert_eq!(import.imported_line(), None);
    }

    #[test]
    fn a_copy_without_a_known_size_still_reports_progress_lines() {
        let mut import = AssetImport::default();
        import.start_copy(None);
        assert!(import.is_busy());
        import.advance_copy(8 * 1024 * 1024);
        assert!(
            import
                .status_note()
                .is_some_and(|note| note.contains("Copying: 8 MB."))
        );
    }

    #[test]
    fn a_known_size_reports_both_numbers() {
        let mut import = AssetImport::default();
        import.start_copy(Some(20 * 1024 * 1024));
        import.advance_copy(5 * 1024 * 1024);
        let note = import.status_note().expect("copying has a note");
        assert!(note.contains("5 of 20 MB"), "{note}");
    }

    #[test]
    fn a_failure_reports_its_reason_and_ends_the_busy_state() {
        let mut import = AssetImport::default();
        import.start_copy(Some(10));
        import.fail("not enough space");
        assert!(!import.is_busy());
        assert!(
            import
                .status_note()
                .is_some_and(|n| n.contains("not enough space"))
        );
    }

    #[test]
    fn a_finished_copy_names_its_folder_and_closing_resets_the_card() {
        let mut import = AssetImport::default();
        import.start_copy(None);
        import.finish_copy("/data/ra2");
        let note = import.status_note().expect("done has a note");
        assert!(note.contains("Restart VERA20K"), "{note}");
        assert!(
            import
                .imported_line()
                .is_some_and(|line| line.contains("/data/ra2"))
        );
        import.close();
        assert_eq!(import.step(), &ImportStep::Closed);
        assert_eq!(import.imported_line(), None);
    }

    #[test]
    fn cancelling_keeps_the_card_busy_until_the_worker_stops() {
        let mut import = AssetImport::default();
        import.start_copy(None);
        import.cancel_copy();
        assert!(import.is_busy());
        assert!(import.status_note().is_some());
    }
}
