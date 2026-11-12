//! A small read-only ZIP reader for the game-file importer.
//!
//! The importer has to unpack what players actually have: a ZIP holding their
//! copy of Yuri's Revenge. The format is read here rather than through a
//! dependency because only two methods occur in such archives (stored and
//! deflate), the entries are ordinary files, and the game already carries an
//! inflater for its own PNG output (`miniz_oxide`). ZIP64 and per-entry
//! encryption are refused with a readable reason instead of being half-read.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};

/// Signature of the end-of-central-directory record.
const EOCD: u32 = 0x0605_4b50;
/// Signature of one central-directory entry.
const CENTRAL: u32 = 0x0201_4b50;
/// Signature of one local file header.
const LOCAL: u32 = 0x0403_4b50;
/// The EOCD record is 22 bytes plus a comment of at most 64 KiB.
const EOCD_SEARCH: u64 = 22 + 0xFFFF;
/// `0xFFFFFFFF` in a 32-bit field means "see the ZIP64 record".
const ZIP64_MARKER: u32 = 0xFFFF_FFFF;
/// Refuse a single member larger than this; the game's own files are far
/// smaller, and a ZIP bomb should fail instead of filling the device.
const MAX_MEMBER_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Names this tool never writes: macOS resource forks and metadata.
const SKIPPED_ROOTS: [&str; 2] = ["__MACOSX/", ".DS_Store"];

/// One member of the archive.
#[derive(Clone, Debug)]
pub(crate) struct Entry {
    /// The stored name, kept as bytes: Windows-made archives often carry GBK
    /// names when the UTF-8 flag is unset, and the game's own files are ASCII.
    name: Vec<u8>,
    /// Where the local file header starts.
    header_offset: u64,
    method: u16,
    compressed: u64,
    uncompressed: u64,
    /// The external attributes' unix mode, when the archive carries one.
    unix_mode: Option<u32>,
}

impl Entry {
    /// Whether this member is a directory record rather than a file.
    pub(crate) fn is_dir(&self) -> bool {
        self.name.ends_with(b"/")
    }

    pub(crate) fn is_symlink(&self) -> bool {
        self.unix_mode
            .is_some_and(|mode| mode & 0o170_000 == 0o120_000)
    }

    /// The path below the extraction root, with any part that could escape it
    /// removed. `None` skips the member.
    pub(crate) fn relative_path(&self) -> Option<PathBuf> {
        let mut name = self.name.clone();
        for byte in &mut name {
            if *byte == b'\\' {
                *byte = b'/';
            }
        }
        let text = String::from_utf8(name).ok()?;
        if SKIPPED_ROOTS
            .iter()
            .any(|skipped| text == *skipped || text.starts_with(skipped))
        {
            return None;
        }
        let mut path = PathBuf::new();
        for part in text.split('/') {
            if part.is_empty() || part == "." {
                continue;
            }
            if part == ".." || part.contains(':') || part.contains('\0') {
                return None;
            }
            path.push(part);
        }
        if path.as_os_str().is_empty() {
            return None;
        }
        // A drive letter or a leading separator never reaches the filesystem.
        if path.components().any(|component| {
            matches!(
                component,
                Component::Prefix(_) | Component::RootDir | Component::ParentDir
            )
        }) {
            return None;
        }
        Some(path)
    }
}

/// An opened archive: the central directory, read once, plus the file it came
/// from.
pub(crate) struct Archive {
    file: File,
    entries: Vec<Entry>,
}

impl Archive {
    pub(crate) fn open(path: &Path) -> Result<Self, String> {
        let mut file = File::open(path).map_err(|error| format!("open: {error}"))?;
        let length = file
            .metadata()
            .map_err(|error| format!("metadata: {error}"))?
            .len();
        let tail = length.min(EOCD_SEARCH);
        file.seek(SeekFrom::Start(length - tail))
            .map_err(|error| format!("seek: {error}"))?;
        let mut buffer = vec![0u8; tail as usize];
        file.read_exact(&mut buffer)
            .map_err(|error| format!("read: {error}"))?;
        let eocd = (0..buffer.len().saturating_sub(21))
            .rev()
            .find(|&at| read_u32(&buffer, at) == Some(EOCD))
            .ok_or_else(|| "no end-of-central-directory record".to_owned())?;
        let total = read_u16(&buffer, eocd + 10).unwrap_or(0);
        let size = read_u32(&buffer, eocd + 12).unwrap_or(0);
        let offset = read_u32(&buffer, eocd + 16).unwrap_or(0);
        if total == 0xFFFF || size == ZIP64_MARKER || offset == ZIP64_MARKER {
            return Err("ZIP64 archives are not supported".to_owned());
        }
        let mut directory = vec![0u8; size as usize];
        file.seek(SeekFrom::Start(u64::from(offset)))
            .map_err(|error| format!("seek: {error}"))?;
        file.read_exact(&mut directory)
            .map_err(|error| format!("read: {error}"))?;
        let mut entries = Vec::with_capacity(total as usize);
        let mut at = 0usize;
        while at + 46 <= directory.len() && read_u32(&directory, at) == Some(CENTRAL) {
            let flags = read_u16(&directory, at + 8).unwrap_or(0);
            let method = read_u16(&directory, at + 10).unwrap_or(0);
            let compressed = read_u32(&directory, at + 20).unwrap_or(0);
            let uncompressed = read_u32(&directory, at + 24).unwrap_or(0);
            let name_len = read_u16(&directory, at + 28).unwrap_or(0) as usize;
            let extra_len = read_u16(&directory, at + 30).unwrap_or(0) as usize;
            let comment_len = read_u16(&directory, at + 32).unwrap_or(0) as usize;
            let unix_mode = read_u32(&directory, at + 38)
                .map(|attributes| attributes >> 16)
                .filter(|mode| *mode != 0);
            let header_offset = read_u32(&directory, at + 42).unwrap_or(0);
            let name_at = at + 46;
            if name_at + name_len > directory.len() {
                return Err("central directory is truncated".to_owned());
            }
            let name = directory[name_at..name_at + name_len].to_vec();
            if flags & 0x1 != 0 {
                return Err("encrypted entries are not supported".to_owned());
            }
            if compressed == ZIP64_MARKER || uncompressed == ZIP64_MARKER {
                return Err("ZIP64 archives are not supported".to_owned());
            }
            entries.push(Entry {
                name,
                header_offset: u64::from(header_offset),
                method,
                compressed: u64::from(compressed),
                uncompressed: u64::from(uncompressed),
                unix_mode,
            });
            at = name_at + name_len + extra_len + comment_len;
        }
        Ok(Self { file, entries })
    }

    pub(crate) fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Total uncompressed size of the files, for the progress line.
    pub(crate) fn total_bytes(&self) -> u64 {
        self.entries
            .iter()
            .filter(|entry| !entry.is_dir() && entry.relative_path().is_some())
            .map(|entry| entry.uncompressed)
            .sum()
    }

    /// One member's bytes.
    pub(crate) fn read(&mut self, entry: &Entry) -> Result<Vec<u8>, String> {
        if entry.uncompressed > MAX_MEMBER_BYTES {
            return Err("member is too large to unpack".to_owned());
        }
        if entry.uncompressed == 0 {
            // An empty member: nothing to inflate, and a zero-sized output
            // buffer would be an inflate error.
            return Ok(Vec::new());
        }
        if entry
            .unix_mode
            .is_some_and(|mode| mode & 0o170_000 == 0o120_000)
        {
            return Err("symbolic links are not unpacked".to_owned());
        }
        self.file
            .seek(SeekFrom::Start(entry.header_offset))
            .map_err(|error| format!("seek: {error}"))?;
        let mut header = [0u8; 30];
        self.file
            .read_exact(&mut header)
            .map_err(|error| format!("read: {error}"))?;
        if u32::from_le_bytes([header[0], header[1], header[2], header[3]]) != LOCAL {
            return Err("local file header is corrupt".to_owned());
        }
        let name_len = u16::from_le_bytes([header[26], header[27]]) as u64;
        let extra_len = u16::from_le_bytes([header[28], header[29]]) as u64;
        self.file
            .seek(SeekFrom::Current((name_len + extra_len) as i64))
            .map_err(|error| format!("seek: {error}"))?;
        let mut compressed = vec![0u8; entry.compressed as usize];
        self.file
            .read_exact(&mut compressed)
            .map_err(|error| format!("read: {error}"))?;
        match entry.method {
            0 => Ok(compressed),
            8 => miniz_oxide::inflate::decompress_to_vec_with_limit(
                &compressed,
                entry.uncompressed as usize,
            )
            .map_err(|error| format!("deflate: {error:?}")),
            other => Err(format!("compression method {other} is not supported")),
        }
    }

    /// Unpack every file below `root`, calling `progress` with the bytes
    /// written so far. Directories are created as needed; symbolic links and
    /// macOS metadata are skipped.
    pub(crate) fn extract(
        &mut self,
        root: &Path,
        mut progress: impl FnMut(u64),
    ) -> Result<(), String> {
        let entries = self.entries.clone();
        let mut written = 0u64;
        for entry in &entries {
            if entry.is_dir() || entry.is_symlink() {
                continue;
            }
            let Some(relative) = entry.relative_path() else {
                continue;
            };
            let data = self.read(entry)?;
            let destination = root.join(&relative);
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|error| format!("create {}: {error}", parent.display()))?;
            }
            // Replace a member that is already there: a re-import overwrites
            // the previous copy of the game.
            std::fs::write(&destination, &data)
                .map_err(|error| format!("write {}: {error}", destination.display()))?;
            written += data.len() as u64;
            progress(written);
        }
        Ok(())
    }
}

fn read_u16(buffer: &[u8], at: usize) -> Option<u16> {
    let bytes = buffer.get(at..at + 2)?;
    Some(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn read_u32(buffer: &[u8], at: usize) -> Option<u32> {
    let bytes = buffer.get(at..at + 4)?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}
