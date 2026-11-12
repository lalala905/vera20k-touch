//! The importer's tests: the ZIP reader, the unpack, the game folder it finds
//! and the config.toml it writes. Everything runs through production code with
//! a ZIP written here, so no retail file is needed.

use super::*;
use crate::assets::asset_manager::test_support::TestDirectory;

/// Write a ZIP with the same two methods a player's archive uses.
fn write_zip(path: &Path, entries: &[(&str, &[u8], bool)]) {
    let mut out: Vec<u8> = Vec::new();
    let mut central: Vec<(String, u16, usize, usize, u32)> = Vec::new();
    for (name, data, deflate) in entries {
        let offset = out.len() as u32;
        let (method, payload) = if *deflate {
            (8u16, miniz_oxide::deflate::compress_to_vec(data, 6))
        } else {
            (0u16, data.to_vec())
        };
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&method.to_le_bytes());
        out.extend_from_slice(&[0u8; 4]);
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&payload);
        central.push((
            (*name).to_owned(),
            method,
            payload.len(),
            data.len(),
            offset,
        ));
    }
    let directory_offset = out.len() as u32;
    let mut directory: Vec<u8> = Vec::new();
    for (name, method, compressed, uncompressed, offset) in &central {
        directory.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        directory.extend_from_slice(&20u16.to_le_bytes());
        directory.extend_from_slice(&20u16.to_le_bytes());
        directory.extend_from_slice(&0u16.to_le_bytes());
        directory.extend_from_slice(&method.to_le_bytes());
        directory.extend_from_slice(&[0u8; 4]);
        directory.extend_from_slice(&0u32.to_le_bytes());
        directory.extend_from_slice(&(*compressed as u32).to_le_bytes());
        directory.extend_from_slice(&(*uncompressed as u32).to_le_bytes());
        directory.extend_from_slice(&(name.len() as u16).to_le_bytes());
        directory.extend_from_slice(&[0u8; 8]);
        directory.extend_from_slice(&0u32.to_le_bytes());
        directory.extend_from_slice(&offset.to_le_bytes());
        directory.extend_from_slice(name.as_bytes());
    }
    let directory_size = directory.len() as u32;
    out.extend_from_slice(&directory);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&[0u8; 4]);
    out.extend_from_slice(&(central.len() as u16).to_le_bytes());
    out.extend_from_slice(&(central.len() as u16).to_le_bytes());
    out.extend_from_slice(&directory_size.to_le_bytes());
    out.extend_from_slice(&directory_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    std::fs::write(path, out).unwrap();
}

fn config_path(data_root: &Path) -> PathBuf {
    data_root.join("config.toml")
}

/// The production import with every progress callback admitting the unpack.
fn import(zip: &Path, data_root: &Path) -> Result<PathBuf, Failure> {
    import_into(zip, data_root, &mut |_| true)
}

#[test]
fn imports_a_zip_that_keeps_the_game_in_a_folder() {
    let temp = TestDirectory::new("game-import-nested");
    let zip = temp.path().join("game.zip");
    write_zip(
        &zip,
        &[
            ("Yuri/ra2md.mix", b"master", false),
            ("Yuri/ra2.mix", b"master", true),
            ("Yuri/rulesmd.ini", b"rules", true),
            ("Yuri/empty.txt", b"", true),
            ("__MACOSX/._ra2md.mix", b"junk", false),
            ("../escape.txt", b"nope", false),
            ("/abs.txt", b"nope", false),
        ],
    );
    let data_root = temp.path().join("data");
    let game = import(&zip, &data_root).expect("the import succeeds");
    assert_eq!(game, data_root.join("ra2").join("Yuri"));
    assert_eq!(std::fs::read(game.join("ra2md.mix")).unwrap(), b"master");
    // Deflated members come back whole, empty ones included.
    assert_eq!(std::fs::read(game.join("rulesmd.ini")).unwrap(), b"rules");
    assert_eq!(std::fs::read(game.join("empty.txt")).unwrap(), b"");
    // Nothing escaped the extraction root, and macOS metadata was skipped.
    assert!(!temp.path().join("escape.txt").exists());
    assert!(!data_root.join("ra2").join("__MACOSX").exists());
    let config = std::fs::read_to_string(config_path(&data_root)).unwrap();
    assert!(
        config.contains(&format!("ra2_dir = \"{}\"", game.display())),
        "{config}"
    );
}

#[test]
fn imports_a_zip_with_the_game_files_at_the_top() {
    let temp = TestDirectory::new("game-import-flat");
    let zip = temp.path().join("game.zip");
    write_zip(
        &zip,
        &[("ra2md.mix", b"m", false), ("ra2.mix", b"m", false)],
    );
    let data_root = temp.path().join("data");
    let game = import(&zip, &data_root).expect("the import succeeds");
    assert_eq!(game, data_root.join("ra2"));
}

#[test]
fn keeps_the_settings_the_import_does_not_own() {
    let temp = TestDirectory::new("game-import-config");
    let data_root = temp.path().join("data");
    std::fs::create_dir_all(&data_root).unwrap();
    std::fs::write(
        config_path(&data_root),
        "[paths]\nra2_dir = \"old\"\nsettings_dir = \"/tmp/settings\"\n",
    )
    .unwrap();
    let zip = temp.path().join("game.zip");
    write_zip(
        &zip,
        &[("ra2md.mix", b"m", false), ("ra2.mix", b"m", false)],
    );
    import(&zip, &data_root).expect("the import succeeds");
    let config = std::fs::read_to_string(config_path(&data_root)).unwrap();
    assert!(
        config.contains("settings_dir = \"/tmp/settings\""),
        "{config}"
    );
    assert!(!config.contains("\"old\""), "{config}");
    assert!(config.contains("ra2_dir ="), "{config}");
}

#[test]
fn a_zip_without_the_master_archives_reports_that() {
    let temp = TestDirectory::new("game-import-missing");
    let zip = temp.path().join("game.zip");
    write_zip(&zip, &[("readme.txt", b"nothing", false)]);
    let data_root = temp.path().join("data");
    assert_eq!(import(&zip, &data_root), Err(Failure::NoGameFiles));
    // The unpacked files stay: the player can pick the right archive next.
    assert!(data_root.join("ra2").join("readme.txt").is_file());
}

#[test]
fn a_file_that_is_not_a_zip_is_refused() {
    let temp = TestDirectory::new("game-import-notzip");
    let zip = temp.path().join("game.zip");
    std::fs::write(&zip, b"not an archive").unwrap();
    let data_root = temp.path().join("data");
    assert!(matches!(
        import(&zip, &data_root),
        Err(Failure::Unreadable(_))
    ));
}

#[test]
fn the_shallowest_folder_with_the_master_archives_wins() {
    let temp = TestDirectory::new("game-import-root");
    let deep = temp.path().join("Game").join("Inner");
    std::fs::create_dir_all(&deep).unwrap();
    std::fs::write(deep.join("ra2md.mix"), b"m").unwrap();
    std::fs::write(deep.join("ra2.mix"), b"m").unwrap();
    let shallow = temp.path().join("Game");
    std::fs::write(shallow.join("ra2md.mix"), b"m").unwrap();
    std::fs::write(shallow.join("ra2.mix"), b"m").unwrap();
    assert_eq!(find_game_root(temp.path()), Some(shallow));
    // Both archives are required: one alone is not a game folder.
    let only_master = temp.path().join("Incomplete");
    std::fs::create_dir_all(&only_master).unwrap();
    std::fs::write(only_master.join("ra2md.mix"), b"m").unwrap();
    assert_ne!(find_game_root(&only_master), Some(only_master));
}

#[test]
fn a_cancelled_unpack_stops_at_the_next_report_and_keeps_no_config() {
    let temp = TestDirectory::new("game-import-cancel");
    let zip = temp.path().join("game.zip");
    write_zip(
        &zip,
        &[("ra2md.mix", b"m", false), ("ra2.mix", b"m", false)],
    );
    let data_root = temp.path().join("data");
    assert_eq!(
        import_into(&zip, &data_root, &mut |_| false),
        Err(Failure::Cancelled)
    );
    assert!(
        !config_path(&data_root).exists(),
        "a cancelled unpack must not point the game at a partial copy"
    );
}
