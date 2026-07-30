//! Where the module library lives, and putting one there.
//!
//! # Why the library is not inside the application
//!
//! The modules and the Macintosh fonts are Berkeley Systems' and Apple's, so
//! they are not shipped with the app and are not in the repository — the person
//! playing supplies their own copy of the original disk once, and this is what
//! unpacks it for them.
//!
//! What comes out goes next to the saved high scores in the platform's
//! application-support directory, **not** into the `.app` bundle. That choice is
//! what makes the import permanent in the way people actually mean it: the app
//! can be moved, renamed, replaced with a newer download, or deleted and
//! reinstalled, and the library is still there. A bundle-relative library would
//! have to be re-imported every time the app was replaced, which is exactly the
//! situation the import exists to avoid.

use std::path::{Path, PathBuf};

use ad_resource::{hfs, ResourceFork};

use crate::save::save_dir;

/// The font files the launcher can draw its own interface with.
///
/// It cannot draw at all without one: the interface is rendered from a real
/// Macintosh strike out of the user's System file, so an import that brought
/// modules but no font would produce an empty window.
const FONT_FILES: &[&str] = &["System", "Chicago", "Geneva", "Monaco"];

/// Where the extracted library lives.
///
/// `None` only when the environment gives no home directory at all.
#[must_use]
pub fn library_dir() -> Option<PathBuf> {
    save_dir().map(|d| d.join("modules"))
}

/// Whether there is a usable library already — at least one module and at least
/// one font.
///
/// Both halves are required because either one missing produces a window the
/// person cannot use, and finding that out at import time is much kinder than
/// finding it out at launch.
#[must_use]
pub fn have_library(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    let (mut module, mut font) = (false, false);
    for path in entries.flatten().map(|e| e.path()) {
        if path.extension().is_none_or(|e| e != "rsrc") {
            continue;
        }
        let stem = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        if FONT_FILES.contains(&stem.as_str()) {
            font = true;
        }
        if !module {
            module = std::fs::read(&path).is_ok_and(|b| is_module(&b));
        }
        if module && font {
            return true;
        }
    }
    false
}

/// What an import produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Installed {
    /// Files carrying an `ADgm` resource — the playable modules.
    pub modules: usize,
    /// Macintosh font files the launcher can draw with.
    pub fonts: usize,
    /// Everything written, including the control panels and documents that come
    /// off the same disk and are kept because they cost nothing.
    pub total: usize,
}

/// Read whatever the person chose and turn it into a library.
///
/// Accepts any of the three things somebody plausibly hands this:
///
/// * a disk image, of any extension — the volume is found by its signature, so
///   `.img`, `.dsk`, `.image` and a bare dump all work;
/// * a folder of already-extracted `.rsrc` files;
/// * a single `.rsrc` file, which imports its whole folder — picking one file
///   inside a folder is how the file chooser lets you name a folder, and it is
///   the same convention "Import scores…" already uses.
///
/// # Errors
///
/// A message written for the person who chose the file, not for a log: it says
/// what was wrong with what they picked and what to do instead.
pub fn install_from(source: &Path) -> Result<Installed, String> {
    let dest = library_dir().ok_or("There is nowhere to keep the modules on this system.")?;
    install_into(source, &dest)
}

/// [`install_from`], with the destination named.
///
/// The product always wants [`library_dir`]; this exists so the import can be
/// tested against a real disk image without reaching for the caller's home
/// directory.
///
/// # Errors
///
/// As [`install_from`].
pub fn install_into(source: &Path, dest: &Path) -> Result<Installed, String> {
    let forks = read_source(source)?;

    if forks.is_empty() {
        return Err(format!(
            "There's nothing in \"{}\".",
            source.file_name().unwrap_or_default().to_string_lossy()
        ));
    }

    // Assembled beside the real library and moved into place only once it is
    // complete, so a failure halfway through cannot leave a half-library that
    // looks importable but is not.
    let staging = dest.to_path_buf().with_extension("incoming");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| format!("Could not write to {}: {e}", staging.display()))?;

    let mut count = Installed { modules: 0, fonts: 0, total: 0 };
    for (name, bytes) in &forks {
        let path = staging.join(format!("{name}.rsrc"));
        if std::fs::write(&path, bytes).is_err() {
            continue;
        }
        count.total = count.total.saturating_add(1);
        if is_module(bytes) {
            count.modules = count.modules.saturating_add(1);
        }
        if FONT_FILES.contains(&name.as_str()) {
            count.fonts = count.fonts.saturating_add(1);
        }
    }

    if count.modules == 0 || count.fonts == 0 {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(missing_halves(count));
    }

    // Only now is the old library replaced.
    let _ = std::fs::remove_dir_all(dest);
    if let Some(parent) = dest.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::rename(&staging, dest)
        .map_err(|e| format!("Could not put the modules in {}: {e}", dest.display()))?;
    Ok(count)
}

/// The message for an import that produced modules but no font, or the reverse.
fn missing_halves(count: Installed) -> String {
    match (count.modules, count.fonts) {
        (0, 0) => "There are no After Dark screen savers in that file.".to_owned(),
        (0, _) => "That's a Mac system disk, not After Dark. There are no screen \
                   savers on it."
            .to_owned(),
        // Fonts and savers travel together on the real thing, so this is
        // somebody who picked a folder holding only the savers.
        _ => "That has the screen savers but not the fonts the menu is drawn with. \
              Choose the whole After Dark download rather than a folder of savers."
            .to_owned(),
    }
}

/// Pull (name, resource-fork bytes) pairs out of whatever was chosen.
fn read_source(source: &Path) -> Result<Vec<(String, Vec<u8>)>, String> {
    if source.is_dir() {
        return Ok(read_folder(source));
    }
    let bytes = std::fs::read(source)
        .map_err(|e| format!("Could not read {}: {e}", source.display()))?;

    // A disk image first: that is what the instructions send people to get.
    match hfs::resource_forks(&bytes) {
        Ok(forks) => {
            return Ok(forks
                .into_iter()
                .map(|f| (f.safe_file_name(), f.data))
                .collect())
        }
        Err(ad_resource::Error::NotAnHfsVolume) => {}
        // It is an After Dark disk, but a damaged one. The parser's own words
        // for that name structures inside the disk format, which mean nothing
        // to the person holding a half-finished download.
        Err(_) => {
            return Err(format!(
                "\"{}\" is damaged, or didn't finish downloading.\n\n\
                 Download it again and choose the new file.",
                source.file_name().unwrap_or_default().to_string_lossy()
            ))
        }
    }

    // Not an image. If it is a resource fork, they pointed at a file inside an
    // already-extracted folder, so take the folder.
    if ResourceFork::parse(&bytes).is_ok() {
        if let Some(folder) = source.parent() {
            let found = read_folder(folder);
            if !found.is_empty() {
                return Ok(found);
            }
        }
    }
    Err(format!(
        "\"{}\" isn't an After Dark download.\n\n\
         If yours arrived as a .zip or .sit file, open it first, then choose \
         the file that comes out.",
        source.file_name().unwrap_or_default().to_string_lossy()
    ))
}

fn read_folder(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for path in entries.flatten().map(|e| e.path()) {
        if path.extension().is_none_or(|e| e != "rsrc") {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let name = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        out.push((name, bytes));
    }
    out
}

/// A file is a module when its resource fork carries `ADgm` — the same test the
/// launcher's own scan uses, and the only reliable one, since nothing in a
/// filename distinguishes a saver from a control panel.
fn is_module(bytes: &[u8]) -> bool {
    ResourceFork::parse(bytes)
        .is_ok_and(|fork| fork.all().iter().any(|r| &r.res_type == b"ADgm"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The library sits beside the saved scores, not inside the application, so
    /// replacing the app cannot lose it.
    #[test]
    fn the_library_is_outside_the_application_bundle() {
        let Some(dir) = library_dir() else { return };
        let text = dir.to_string_lossy().into_owned();
        assert!(!text.contains(".app/"), "library must not be inside a bundle: {text}");
        assert!(dir.ends_with("modules"));
        assert_eq!(dir.parent().map(Path::to_path_buf), save_dir());
    }

    /// An empty or absent directory is not a library, and asking does not panic.
    #[test]
    fn an_absent_directory_is_not_a_library() {
        assert!(!have_library(Path::new("/no/such/directory/at/all")));
    }

    /// Bytes that are not a resource fork are not a module.
    #[test]
    fn junk_is_not_a_module() {
        assert!(!is_module(&[]));
        assert!(!is_module(b"this is not a resource fork"));
    }

    /// The whole import, against a real disk image.
    ///
    /// Skipped rather than failed when the image is absent: it is the user's
    /// licensed copy and is not in the repository, so a contributor without one
    /// still gets every other test. Same trade the compatibility survey makes.
    #[test]
    fn a_real_disk_image_imports_into_a_usable_library() {
        let image = Path::new("../../AfterDark-original.img");
        if !image.exists() {
            return;
        }
        let dest = std::env::temp_dir().join("ad-library-test-install/modules");
        let _ = std::fs::remove_dir_all(dest.parent().unwrap_or(&dest));

        let count = install_into(image, &dest).expect("the image imports");
        assert!(count.modules > 50, "expected the full module set, got {count:?}");
        assert!(count.fonts > 0, "the launcher cannot draw without a font");
        assert!(have_library(&dest), "an imported library must be usable");

        // Nothing is left behind beside it.
        assert!(
            !dest.with_extension("incoming").exists(),
            "staging directory must be gone once the import lands"
        );

        // Importing the resulting folder is the "already extracted" route, and
        // must produce the same library rather than a partial one.
        let again = std::env::temp_dir().join("ad-library-test-install-2/modules");
        let _ = std::fs::remove_dir_all(again.parent().unwrap_or(&again));
        let from_folder = install_into(&dest, &again).expect("a folder of forks imports");
        assert_eq!(from_folder.total, count.total);
        assert!(have_library(&again));

        // And picking a single file inside that folder means the folder.
        let one = dest.join("Flying Toasters.rsrc");
        if one.exists() {
            let third = std::env::temp_dir().join("ad-library-test-install-3/modules");
            let _ = std::fs::remove_dir_all(third.parent().unwrap_or(&third));
            let from_file = install_into(&one, &third).expect("one file means its folder");
            assert_eq!(from_file.total, count.total);
            let _ = std::fs::remove_dir_all(third.parent().unwrap_or(&third));
        }

        let _ = std::fs::remove_dir_all(dest.parent().unwrap_or(&dest));
        let _ = std::fs::remove_dir_all(again.parent().unwrap_or(&again));
    }

    /// Importing a library must not disturb the high scores it sits beside.
    ///
    /// The library is a `modules` subdirectory *of the save directory*, so an
    /// import runs `remove_dir_all` one level below somebody's saved games.
    /// Getting that path wrong by one component would delete every high score
    /// on the machine, silently, at the moment they were being helpful.
    #[test]
    fn importing_a_library_leaves_the_high_scores_alone() {
        let image = Path::new("../../AfterDark-original.img");
        if !image.exists() {
            return;
        }
        let saves = std::env::temp_dir().join("ad-library-test-coexist");
        let _ = std::fs::remove_dir_all(&saves);
        std::fs::create_dir_all(&saves).expect("mkdir");

        // A save sitting where a real one would, and its exact bytes.
        let score = saves.join("Lunatic Fringe.save.rsrc");
        let original = b"a high score nobody wants to lose".to_vec();
        std::fs::write(&score, &original).expect("write save");

        // The library goes in `modules` beneath it — twice, so the second
        // import exercises the remove-and-replace path with a save present.
        let library = saves.join("modules");
        install_into(image, &library).expect("first import");
        assert!(score.is_file(), "the save must survive an import");
        install_into(image, &library).expect("re-import over an existing library");

        assert_eq!(
            std::fs::read(&score).expect("the save is still readable"),
            original,
            "re-importing must not touch the save's contents"
        );
        assert!(have_library(&library));

        // And exporting scores still finds the save and ignores the library.
        let out = std::env::temp_dir().join("ad-library-test-coexist-export");
        let _ = std::fs::remove_dir_all(&out);
        let exported = crate::save::export_from_into(&saves, &out).expect("export");
        assert_eq!(exported, 1, "exactly the one save, not the library");
        assert!(out.join("Lunatic Fringe.save.rsrc").is_file());
        assert!(!out.join("modules").exists(), "the library is not a score");

        let _ = std::fs::remove_dir_all(&saves);
        let _ = std::fs::remove_dir_all(&out);
    }

    /// A library with modules but no font is refused at import time, with the
    /// staging directory cleaned up — finding this out at launch, in a window
    /// that cannot draw a single character, is much worse.
    #[test]
    fn an_import_with_no_font_is_refused_and_leaves_nothing_behind() {
        let source = std::env::temp_dir().join("ad-library-test-nofont-src");
        let dest = std::env::temp_dir().join("ad-library-test-nofont/modules");
        let _ = std::fs::remove_dir_all(&source);
        let _ = std::fs::remove_dir_all(dest.parent().unwrap_or(&dest));
        std::fs::create_dir_all(&source).expect("mkdir");
        // Not a real fork, so it is neither a module nor a font.
        std::fs::write(source.join("Nonsense.rsrc"), b"not a fork").expect("write");

        let err = install_into(&source, &dest).expect_err("no modules, no font");
        assert!(err.contains("no After Dark screen savers"), "{err}");
        assert!(!dest.exists(), "a refused import must not create the library");
        assert!(!dest.with_extension("incoming").exists(), "staging must be cleaned up");

        let _ = std::fs::remove_dir_all(&source);
        let _ = std::fs::remove_dir_all(dest.parent().unwrap_or(&dest));
    }

    /// Choosing something that is neither an image nor a fork explains itself
    /// rather than failing with a parser's vocabulary.
    #[test]
    fn a_file_that_is_not_an_image_says_what_to_do() {
        let path = std::env::temp_dir().join("ad-library-test-not-an-image.txt");
        std::fs::write(&path, b"hello").expect("write temp file");
        let err = read_source(&path).expect_err("plain text is not importable");
        // Names the file they picked, and says what to do next.
        assert!(err.contains("ad-library-test-not-an-image.txt"), "{err}");
        assert!(err.contains("isn't an After Dark download"), "{err}");
        assert!(err.contains("open it first"), "{err}");
        let _ = std::fs::remove_file(&path);
    }

    /// Nothing a player is shown may use our vocabulary for our problems.
    ///
    /// Every message in this module goes into a dialog box in front of somebody
    /// who wants to run a screen saver from 1991. "Disk image", "resource fork"
    /// and "HFS volume" are how *this codebase* refers to things, and each one
    /// shipped in a user-facing string at some point before this test existed.
    #[test]
    fn no_message_shown_to_a_player_uses_our_vocabulary() {
        let jargon = [
            "disk image",
            "resource fork",
            "HFS",
            "volume",
            "fork",
            "module",
            "MacBinary",
            "extent",
            "parse",
            "malformed",
        ];
        // Every `Err(...)` string this module can produce, gathered by hand —
        // there is no reflection to enumerate them with.
        let shown = [
            missing_halves(Installed { modules: 0, fonts: 0, total: 1 }),
            missing_halves(Installed { modules: 0, fonts: 1, total: 1 }),
            missing_halves(Installed { modules: 1, fonts: 0, total: 1 }),
            install_into(Path::new("/no/such/file"), Path::new("/tmp/x")).unwrap_err(),
        ];
        for message in &shown {
            let lower = message.to_lowercase();
            for word in jargon {
                assert!(
                    !lower.contains(&word.to_lowercase()),
                    "{word:?} is our word, not theirs, and it is in: {message:?}"
                );
            }
        }
    }
}
