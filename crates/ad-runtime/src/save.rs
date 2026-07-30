//! Durable saved state: where it goes, and how it gets there intact.
//!
//! # The overlay
//!
//! A module's saved state is written to its **own** resource fork, separate from
//! the module file, holding only the resources the module added or changed. The
//! original is never rewritten: it is the user's licensed copy, it is opened
//! read-only, and a save that could corrupt it would be a save worth not having.
//!
//! At load time the overlay is parsed and merged over the module's resources, so
//! `_Get1Resource('LFhs', 128)` returns the saved high score rather than the
//! shipped default. That merge uses the same parser as everything else, because
//! the overlay *is* a resource fork — see [`ad_resource::write_fork`].
//!
//! # Atomicity
//!
//! Write to a temporary file in the same directory, flush it, `fsync` it, then
//! `rename` over the target. `rename` within a directory is atomic on every
//! platform this ships to, so a process killed at any point leaves either the
//! previous save or the new one. A high score is fully present or cleanly
//! absent, never half-written — which is the property that makes it safe to
//! save from inside a game's frame loop.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use ad_resource::{OwnedResource, ResourceFork};
use ad_toolbox::resources::{ResourceSink, StoredResource};

/// The per-platform directory for saved module state.
///
/// * macOS: `~/Library/Application Support/After Dark/`
/// * Windows: `%APPDATA%\After Dark\`
/// * elsewhere: `$XDG_DATA_HOME/after-dark/`, else `~/.local/share/after-dark/`
///
/// `None` when the environment gives no home directory at all, in which case the
/// honest answer is "nowhere to save" rather than a path in the current working
/// directory that the user will never find.
#[must_use]
pub fn save_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME")?;
        Some(
            PathBuf::from(home)
                .join("Library/Application Support")
                .join("After Dark"),
        )
    }
    #[cfg(target_os = "windows")]
    {
        let appdata = std::env::var_os("APPDATA")?;
        Some(PathBuf::from(appdata).join("After Dark"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        if let Some(x) = std::env::var_os("XDG_DATA_HOME") {
            if !x.is_empty() {
                return Some(PathBuf::from(x).join("after-dark"));
            }
        }
        let home = std::env::var_os("HOME")?;
        Some(PathBuf::from(home).join(".local/share/after-dark"))
    }
}

/// A filesystem-safe, stable, human-recognisable filename for a module.
///
/// Keyed on the module's **title**, not a hash of its bytes. A content hash
/// would be tidier but it orphans the user's high scores the moment the module
/// file differs by a byte, and the point of saving is that the score is still
/// there next time. Bytes outside a conservative set become `_`, and the result
/// is truncated so no path length limit is reached.
#[must_use]
pub fn file_stem_for(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    for ch in title.chars().take(64) {
        if ch.is_ascii_alphanumeric() || ch == ' ' || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    let trimmed = out.trim().to_owned();
    if trimmed.is_empty() {
        "module".to_owned()
    } else {
        trimmed
    }
}

/// Durable resource writes for one module, as an overlay fork.
#[derive(Debug)]
pub struct ForkSink {
    path: PathBuf,
}

impl ForkSink {
    /// A sink writing `<dir>/<title>.save.rsrc`.
    ///
    /// The directory is created on first write, not here: a module that never
    /// saves should not cause a directory to appear.
    #[must_use]
    pub fn new(dir: &Path, title: &str) -> Self {
        Self {
            path: dir.join(format!("{}.save.rsrc", file_stem_for(title))),
        }
    }

    /// Where this sink writes.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Load a previously saved overlay, if there is one.
    ///
    /// A corrupt or truncated overlay is reported as an error and **not** merged:
    /// the module then starts from its shipped defaults, which is a recoverable
    /// state, rather than from resources assembled out of whatever bytes
    /// survived.
    ///
    /// # Errors
    /// The reason the file could not be read or parsed.
    pub fn load(dir: &Path, title: &str) -> Result<Vec<StoredResource>, String> {
        let path = dir.join(format!("{}.save.rsrc", file_stem_for(title)));
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let fork = ResourceFork::parse(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(fork
            .all()
            .iter()
            .map(|r| StoredResource {
                res_type: r.res_type,
                id: r.id,
                name: r.name.clone(),
                name_bytes: r.name_bytes.map(<[u8]>::to_vec),
                attrs: r.attrs,
                data: r.data.to_vec(),
            })
            .collect())
    }
}

impl ResourceSink for ForkSink {
    fn persist(&mut self, changed: &[StoredResource]) -> Result<(), String> {
        let owned: Vec<OwnedResource> = changed
            .iter()
            .map(|e| OwnedResource {
                res_type: e.res_type,
                id: e.id,
                name_bytes: e.name_bytes.clone(),
                attrs: e.attrs,
                data: e.data.clone(),
            })
            .collect();
        let bytes = ad_resource::write_fork(&owned).map_err(|e| e.to_string())?;

        write_atomically(&self.path, &bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ad-runtime-test-{name}"));
        let _ = fs::remove_dir_all(&d);
        d
    }

    fn stored(ty: &[u8; 4], id: i16, name: Option<&str>, data: &[u8]) -> StoredResource {
        StoredResource::synthetic(*ty, id, name, data.to_vec())
    }

    #[test]
    fn a_saved_high_score_comes_back() {
        let dir = scratch("highscore");
        let mut sink = ForkSink::new(&dir, "Lunatic Fringe");
        let score = stored(b"LFhs", 128, Some("High Scores"), &[0xDE, 0xAD, 0xBE, 0xEF]);
        sink.persist(std::slice::from_ref(&score)).expect("persist");

        let back = ForkSink::load(&dir, "Lunatic Fringe").expect("load");
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].data, score.data);
        assert_eq!(back[0].res_type, *b"LFhs");
        assert_eq!(back[0].id, 128);
        assert_eq!(back[0].name.as_deref(), Some("High Scores"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_save_file_is_not_an_error() {
        let dir = scratch("absent");
        assert!(ForkSink::load(&dir, "Never Ran").expect("load").is_empty());
    }

    #[test]
    fn a_corrupt_overlay_is_refused_not_partially_believed() {
        let dir = scratch("corrupt");
        fs::create_dir_all(&dir).unwrap();
        let sink = ForkSink::new(&dir, "Broken");
        fs::write(sink.path(), b"not a resource fork at all").unwrap();
        assert!(ForkSink::load(&dir, "Broken").is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_second_save_replaces_the_first_and_leaves_no_temp_file() {
        let dir = scratch("replace");
        let mut sink = ForkSink::new(&dir, "Twice");
        sink.persist(&[stored(b"LFhs", 1, None, &[1])]).unwrap();
        sink.persist(&[stored(b"LFhs", 1, None, &[2, 2, 2])]).unwrap();
        let back = ForkSink::load(&dir, "Twice").unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].data, vec![2, 2, 2]);
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp files left behind: {leftovers:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_previous_save_survives_a_failed_write() {
        // The atomicity contract, exercised the only way it can be without
        // killing a process: make the *new* write fail (a duplicate cannot be
        // serialised) and check the old file is still intact and still loadable.
        let dir = scratch("failed");
        let mut sink = ForkSink::new(&dir, "Durable");
        sink.persist(&[stored(b"LFhs", 1, None, &[7])]).unwrap();
        let err = sink
            .persist(&[stored(b"LFhs", 1, None, &[8]), stored(b"LFhs", 1, None, &[9])])
            .expect_err("a duplicate must not be written");
        assert!(err.contains("duplicate"), "{err}");
        let back = ForkSink::load(&dir, "Durable").unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].data, vec![7], "the earlier save must be intact");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn titles_become_safe_filenames_without_colliding_on_the_obvious_cases() {
        assert_eq!(file_stem_for("Lunatic Fringe"), "Lunatic Fringe");
        assert_eq!(file_stem_for("Movies 'Til Dawn"), "Movies _Til Dawn");
        assert_eq!(file_stem_for("../../etc/passwd"), "______etc_passwd");
        assert_eq!(file_stem_for(""), "module");
        assert_eq!(file_stem_for("   "), "module");
        // A path separator must never survive, on either platform.
        for stem in ["a/b", "a\\b", "a:b"] {
            let s = file_stem_for(stem);
            assert!(!s.contains('/') && !s.contains('\\') && !s.contains(':'), "{s}");
        }
    }
}

/// Copy every saved module file into `to`, returning how many were written.
///
/// # Errors
/// A message naming what failed. Nothing partial is hidden: a copy that fails
/// part-way reports the file it stopped on, because a backup that silently missed
/// one module is worse than one that says so.
///
/// The files are the saved resource forks themselves, copied verbatim rather than
/// repackaged. That is what makes an export portable — it can be copied back into
/// the save directory on another machine, or on another platform, and be read by
/// any version of this runtime.
pub fn export_scores(to: &Path) -> Result<usize, String> {
    let from = save_dir().ok_or("no save directory on this platform")?;
    export_from_into(&from, to)
}

/// The whole of [`export_scores`] with the source named, so the tests drive the
/// real code path instead of a copy of it — and so they do not depend on what
/// happens to be in the developer's own save directory, which is what made the
/// "nothing saved yet" test start failing the first time somebody actually
/// finished a game on the machine running it.
pub(crate) fn export_from_into(from: &Path, to: &Path) -> Result<usize, String> {
    let entries = match std::fs::read_dir(from) {
        Ok(e) => e,
        // Never having saved is not an error, it is a count of zero.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(format!("{}: {e}", from.display())),
    };
    std::fs::create_dir_all(to).map_err(|e| format!("{}: {e}", to.display()))?;
    let mut written = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "rsrc") {
            continue;
        }
        let Some(name) = path.file_name() else { continue };
        let dest = to.join(name);
        // Refuse rather than copy a file onto itself, which truncates it.
        if dest == path {
            return Err("that is the folder the scores are already in".to_owned());
        }
        std::fs::copy(&path, &dest).map_err(|e| format!("{}: {e}", name.to_string_lossy()))?;
        written = written.saturating_add(1);
    }
    Ok(written)
}

/// What an import did, in enough detail to tell the person the truth.
///
/// `replaced` is counted separately from `added` because importing over a save
/// **destroys the score that was there**. A single "imported 3 files" would hide
/// that, and the one thing somebody restoring a backup needs to know is whether
/// they just overwrote a better score than the one in the file.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Imported {
    /// Saves for modules that had none here before.
    pub added: usize,
    /// Saves that replaced an existing one.
    pub replaced: usize,
    /// Files that were not usable saves, with the reason, in directory order.
    pub rejected: Vec<String>,
}

impl Imported {
    /// Total files brought in.
    #[must_use]
    pub fn total(&self) -> usize {
        self.added.saturating_add(self.replaced)
    }
}

/// Copy saved module state from `from` into the save directory.
///
/// The inverse of [`export_scores`], and deliberately not a plain directory copy:
/// **every file is parsed as a resource fork before it is allowed in**. A save
/// that does not parse is refused by name and the rest still import, because the
/// alternative is a file that only fails later — at which point
/// [`ForkSink::load`] reports a corrupt overlay and the module silently starts
/// from its shipped defaults, and the user's real scores are already gone.
///
/// Files are written through the same temp-then-rename dance as a live save, so
/// an import interrupted half-way leaves each module's score either wholly
/// replaced or wholly untouched.
///
/// # Errors
/// A message naming what failed, for conditions that stop the whole import: no
/// save directory on this platform, an unreadable source folder, or the source
/// being the save directory itself.
pub fn import_scores(from: &Path) -> Result<Imported, String> {
    let to = save_dir().ok_or("no save directory on this platform")?;
    import_into(from, &to)
}

/// The whole of [`import_scores`] with the destination named, so the tests drive
/// the real code path instead of a copy of it.
///
/// `from` may be a single exported file as well as a folder of them: the file
/// chooser hands back whichever the person clicked, and someone who selects
/// `Lunatic Fringe.save.rsrc` means that file, not a lecture about folders.
fn import_into(from: &Path, to: &Path) -> Result<Imported, String> {
    let mut files: Vec<PathBuf>;
    if from.is_file() {
        if from.parent() == Some(to) {
            return Err("that file is already in the scores folder".to_owned());
        }
        files = vec![from.to_path_buf()];
    } else {
        let entries = match std::fs::read_dir(from) {
            Ok(e) => e,
            Err(e) => return Err(format!("{}: {e}", from.display())),
        };
        // Copying the save directory onto itself would truncate every file in it.
        if from == to {
            return Err("that is the folder the scores are already in".to_owned());
        }
        // Sorted, so "the third file was rejected" means the same thing on every
        // platform; `read_dir` order is not defined.
        files = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "rsrc"))
            .collect();
        files.sort();
    }
    let mut report = Imported::default();
    for path in files {
        let Some(name) = path.file_name().map(std::ffi::OsStr::to_os_string) else {
            continue;
        };
        let shown = name.to_string_lossy().into_owned();
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                report.rejected.push(format!("{shown}: {e}"));
                continue;
            }
        };
        if let Err(e) = ResourceFork::parse(&bytes) {
            report.rejected.push(format!("{shown}: not a saved score ({e})"));
            continue;
        }
        let dest = to.join(&name);
        let existed = dest.exists();
        if let Err(e) = write_atomically(&dest, &bytes) {
            report.rejected.push(format!("{shown}: {e}"));
            continue;
        }
        if existed {
            report.replaced = report.replaced.saturating_add(1);
        } else {
            report.added = report.added.saturating_add(1);
        }
    }
    Ok(report)
}

/// Write `bytes` to `path` via a temporary file in the same directory.
///
/// The atomicity the module header promises, in one place: the temp file is a
/// sibling of the target so the rename cannot cross a filesystem boundary and
/// stop being atomic, and it is `fsync`ed first — without that the rename can
/// land while the data is still in the page cache, so a power loss leaves a
/// correctly-named empty file, the one outcome the rename was there to rule out.
///
/// Shared by [`ForkSink::persist`] and [`import_scores`] so an imported save
/// lands exactly as durably as one the module wrote itself.
pub(crate) fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let dir = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let tmp = path.with_extension("rsrc.tmp");
    {
        let mut f = fs::File::create(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
        f.write_all(bytes)
            .map_err(|e| format!("{}: {e}", tmp.display()))?;
        f.sync_all().map_err(|e| format!("{}: {e}", tmp.display()))?;
    }
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("{}: {e}", path.display())
    })
}

#[cfg(test)]
mod export_tests {
    use super::*;

    /// An export copies the saves verbatim and counts them.
    #[test]
    fn exporting_copies_every_save_and_ignores_anything_else() {
        let root = std::env::temp_dir().join("ad-export-test");
        let _ = std::fs::remove_dir_all(&root);
        let (from, to) = (root.join("saves"), root.join("out"));
        std::fs::create_dir_all(&from).expect("mkdir");
        std::fs::write(from.join("Lunatic Fringe.save.rsrc"), b"scores").expect("write");
        std::fs::write(from.join("Life II.save.rsrc"), b"state").expect("write");
        // Not a save, and must not be swept up.
        std::fs::write(from.join("notes.txt"), b"ignore me").expect("write");

        let written = export_from_into(&from, &to).expect("export");
        assert_eq!(written, 2, "both saves, and not the text file");
        assert_eq!(
            std::fs::read(to.join("Lunatic Fringe.save.rsrc")).expect("read"),
            b"scores",
            "copied verbatim, so it can be copied back"
        );
        assert!(!to.join("notes.txt").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A real fork, so the import validator has something it should accept.
    fn a_real_save(score: &[u8]) -> Vec<u8> {
        ad_resource::write_fork(&[OwnedResource {
            res_type: *b"LFhs",
            id: 128,
            name_bytes: Some(b"High Scores".to_vec()),
            attrs: 0,
            data: score.to_vec(),
        }])
        .expect("write a fork")
    }

    /// The round trip the buttons exist for: export, then import somewhere else.
    #[test]
    fn importing_accepts_real_saves_and_says_which_it_replaced() {
        let root = std::env::temp_dir().join("ad-import-test");
        let _ = std::fs::remove_dir_all(&root);
        let (from, to) = (root.join("backup"), root.join("saves"));
        std::fs::create_dir_all(&from).expect("mkdir");
        std::fs::create_dir_all(&to).expect("mkdir");

        std::fs::write(from.join("Lunatic Fringe.save.rsrc"), a_real_save(&[1, 2, 3])).unwrap();
        std::fs::write(from.join("Life II.save.rsrc"), a_real_save(&[9])).unwrap();
        // Neither of these may be imported: one is not a fork, one is not a save.
        std::fs::write(from.join("Broken.save.rsrc"), b"not a resource fork").unwrap();
        std::fs::write(from.join("notes.txt"), b"ignore me").unwrap();
        // Already here, so importing over it is a *replacement*, not an addition.
        std::fs::write(to.join("Lunatic Fringe.save.rsrc"), a_real_save(&[0])).unwrap();

        let report = import_into(&from, &to).expect("import");
        assert_eq!(report.added, 1, "Life II was new");
        assert_eq!(report.replaced, 1, "Lunatic Fringe was already there");
        assert_eq!(report.total(), 2);
        assert_eq!(report.rejected.len(), 1, "the corrupt fork, and only it");
        assert!(report.rejected[0].starts_with("Broken.save.rsrc"), "{:?}", report.rejected);

        // The imported bytes are readable as the module's own saved state.
        let back = ForkSink::load(&to, "Lunatic Fringe").expect("load");
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].data, vec![1, 2, 3], "the backup's score, not the old one");
        assert!(!to.join("notes.txt").exists(), "only .rsrc files are considered");
        // A refused file must not have been written in any form.
        assert!(!to.join("Broken.save.rsrc").exists());
        let leftovers: Vec<_> = std::fs::read_dir(&to)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp files left behind: {leftovers:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A single exported file imports as itself, and only itself.
    #[test]
    fn importing_one_chosen_file_brings_in_that_file_alone() {
        let root = std::env::temp_dir().join("ad-import-onefile");
        let _ = std::fs::remove_dir_all(&root);
        let (from, to) = (root.join("backup"), root.join("saves"));
        std::fs::create_dir_all(&from).expect("mkdir");
        std::fs::write(from.join("Lunatic Fringe.save.rsrc"), a_real_save(&[5])).unwrap();
        std::fs::write(from.join("Life II.save.rsrc"), a_real_save(&[6])).unwrap();

        let report =
            import_into(&from.join("Lunatic Fringe.save.rsrc"), &to).expect("import one file");
        assert_eq!((report.added, report.replaced), (1, 0));
        assert!(to.join("Lunatic Fringe.save.rsrc").exists());
        assert!(
            !to.join("Life II.save.rsrc").exists(),
            "choosing one file must not sweep in its neighbours"
        );

        // A file already in the save folder is refused, same as the folder case.
        let err = import_into(&to.join("Lunatic Fringe.save.rsrc"), &to).expect_err("refuse");
        assert!(err.contains("already in"), "{err}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Importing a folder into itself would truncate every file in it.
    #[test]
    fn importing_the_save_directory_into_itself_is_refused() {
        let dir = std::env::temp_dir().join("ad-import-self");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("A.save.rsrc"), a_real_save(&[7])).unwrap();
        let err = import_into(&dir, &dir).expect_err("must refuse");
        assert!(err.contains("already in"), "{err}");
        assert_eq!(
            std::fs::read(dir.join("A.save.rsrc")).unwrap(),
            a_real_save(&[7]),
            "the refusal must not have touched anything"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A folder with nothing importable in it is a count of zero, not a failure.
    #[test]
    fn importing_an_empty_folder_reports_nothing_rather_than_failing() {
        let root = std::env::temp_dir().join("ad-import-empty");
        let _ = std::fs::remove_dir_all(&root);
        let (from, to) = (root.join("empty"), root.join("saves"));
        std::fs::create_dir_all(&from).expect("mkdir");
        let report = import_into(&from, &to).expect("import");
        assert_eq!(report, Imported::default());
        assert_eq!(report.total(), 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Never having saved anything exports zero files rather than failing.
    #[test]
    fn an_absent_save_directory_is_zero_not_an_error() {
        let root = std::env::temp_dir().join("ad-export-empty");
        let _ = std::fs::remove_dir_all(&root);
        let (never_saved, to) = (root.join("nothing here"), root.join("out"));
        assert_eq!(
            export_from_into(&never_saved, &to).expect("a missing source is not an error"),
            0
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
