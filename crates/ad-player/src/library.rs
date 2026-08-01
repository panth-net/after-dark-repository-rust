//! Finding the modules on disk.
//!
//! A file is a module when its resource fork contains an `ADgm` code resource.
//! That is the same test the compatibility lab uses, and it is the only reliable
//! one: the After Dark disk holds fonts, control panels, the Finder and a dozen
//! other forks alongside the savers, and nothing in a filename distinguishes
//! them.
//!
//! This is also what Randomizer and MultiModule need. Both decline today with
//! "a file error occurred" because they expect to enumerate their siblings, and
//! a launcher that knows the library is the first half of giving them one.

use ad_resource::{AdModule, ModuleSettings, ResourceFork};
use std::path::{Path, PathBuf};

/// One module the launcher can offer.
#[derive(Debug, Clone)]
pub struct Entry {
    pub path: PathBuf,
    /// What the list shows: the **filename**. See [`scan`] for why not the
    /// module's own descriptor.
    pub title: String,
    /// The module's `ADrk 0` descriptor line — name, version, copyright — for the
    /// details panel. `None` for the 26 modules on the disk that have no `ADrk 0`
    /// at all.
    pub descriptor: Option<String>,
    /// Its settings, decoded, for the details panel.
    pub controls: Vec<String>,
    /// Number of resources, and whether it carries sound.
    pub resources: usize,
    pub has_sound: bool,
}

/// Modules whose own `Initialize` always refuses with "This module requires
/// After Dark 3.0 or later." — traced to a
/// `_HFSDispatch` call hunting the disk for the real After Dark 3.0 engine
/// (`AD 3.0 Code`/`AD 3.0 Sound`), which this runtime has no file system to
/// supply. Not a bug in the module and not fixable by anything short of
/// hosting that engine, so listing them next to modules that actually play
/// would just be a Play button that always fails. Names are filenames (see
/// [`scan`]), taken from the evidence in `tests/baseline/modules.json`.
const NEEDS_AFTER_DARK_3: &[&str] = &[
    "Bad Dog",
    "Bugs",
    "Bungee Roulette",
    "Chameleon",
    "Clocks",
    "Coming Soon",
    "Fish Pro",
    "Flying Toasters Pro",
    "Flying Toilets",
    "FrankenScreen",
    "Message Mayhem",
    "Mike's So-called Life",
    "Mime Hunt",
    "Mowin' Boris",
    "Nirvana",
    "Phlegm Boy",
    "Rat Race",
    "Shock Clocks",
    "Toxic Swamp",
    "Voyeur",
    "You Bet Your Head",
];

/// Modules the compatibility survey says do not play: they refuse, they need a
/// Toolbox trap this runtime has no implementation for, or they complete their
/// whole lifecycle without drawing a single pixel.
///
/// Kept out of the list for the same reason as [`NEEDS_AFTER_DARK_3`] — offering
/// somebody a Play button that yields a black screen is worse than not offering
/// it. This is the filter the packaged app used to apply when it was *built*,
/// which no longer works now that the library is imported at run time from the
/// user's own disk rather than assembled by the packaging script.
///
/// Derived wholly from `tests/baseline/modules.json`, and
/// `the_skip_lists_match_the_survey_baseline` fails if the two drift apart.
const DOES_NOT_PLAY: &[&str] = &[
    "Artist",
    "Blackboard",
    "DOS Shell",
    "DrawMorph",
    "Gravity",
    "Marbles",
    "Meadow",
    "Messages",
    "MonitorSD",
    "Movies 'Til Dawn",
    "MultiModule",
    "Nocturnes",
    "Nonsense",
    "PICS Player",
    "Photon",
    "Picture Frame",
    "Punchout",
    "Puzzle",
    "Randomizer",
    "Say What_",
    "Slide Show",
    "Spotlight",
    "Terraform",
];

/// Scan a directory for modules, sorted by name.
///
/// Unreadable and unparseable files are skipped in silence: the directory is a
/// dump of a 1991 disk, most of it is not a module, and a launcher that reported
/// every non-module as an error would be unusable. Modules in
/// [`NEEDS_AFTER_DARK_3`] and [`DOES_NOT_PLAY`] are skipped the same way,
/// deliberately: they are real modules, correctly read, that this runtime
/// cannot run.
///
/// # The list shows filenames, not the modules' own titles
///
/// The first version of this used `AdModule::title()`, the first line of the
/// `ADrk 0` descriptor, and the rendered list came out with **five rows all
/// reading "Flying Toasters 2.0"**. That is not a decoding bug: Bogglins, Flying
/// Toasters, Major Metaphysical Appliances, Pearls and ProtoToasters all ship
/// with the same copy-pasted descriptor on the original disk. A further 26
/// modules have no `ADrk 0` at all.
///
/// Filenames are unique by construction and are what the user sees in the
/// Finder, which is also what After Dark's own control panel listed. The
/// descriptor is kept and shown in the details panel, where a shared or absent
/// one is a visible fact about the module instead of an unusable list.
#[must_use]
pub fn scan(dir: &Path) -> Vec<Entry> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<Entry> = Vec::new();
    for item in read.flatten() {
        let path = item.path();
        if path.extension().is_none_or(|e| e != "rsrc") {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let Ok(fork) = ResourceFork::parse(&bytes) else {
            continue;
        };
        if fork.all().iter().all(|r| &r.res_type != b"ADgm") {
            continue;
        }
        let resources = fork.len();
        let has_sound = fork.all().iter().any(|r| &r.res_type == b"snd ");
        let settings = ModuleSettings::from_fork(&fork);
        let controls = describe(&settings);
        let descriptor = AdModule::new(fork).title().filter(|t| !t.is_empty());
        let title = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        if NEEDS_AFTER_DARK_3.contains(&title.as_str()) || DOES_NOT_PLAY.contains(&title.as_str()) {
            continue;
        }
        out.push(Entry {
            path,
            title,
            descriptor,
            controls,
            resources,
            has_sound,
        });
    }
    out.sort_by_key(|a| a.title.to_lowercase());
    out
}

/// One line per control, as the settings panel shows them.
///
/// A slider's number means nothing on its own — After Dark showed the matching
/// `sUnt` label, so "Flying objects: 12" rather than "Flying objects: 60".
fn describe(settings: &ModuleSettings) -> Vec<String> {
    use ad_resource::Control;
    settings
        .controls
        .iter()
        .flatten()
        .map(|c| match c {
            Control::Slider {
                label,
                value,
                units,
            } => {
                let name = label.as_deref().unwrap_or("Slider");
                // The unit whose lower limit the value has reached.
                let text = units
                    .iter()
                    .rfind(|u| *value >= u.lower_limit)
                    .map(|u| u.text.clone());
                match text {
                    Some(t) => format!("{name} {t}"),
                    None => format!("{name} {value}"),
                }
            }
            Control::CheckBox { label, checked } => format!(
                "{} {}",
                label.as_deref().unwrap_or("Option"),
                if *checked { "on" } else { "off" }
            ),
            Control::Menu { label, value } => {
                format!("{} #{value}", label.as_deref().unwrap_or("Menu"))
            }
            Control::Button { label, message } => {
                format!("[{}] sends {message}", label.as_deref().unwrap_or("Button"))
            }
            Control::Text { label, .. } => label.as_deref().unwrap_or("Text").to_owned(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// The two skip lists together must be exactly what the survey says does not
    /// play — no more, no less.
    ///
    /// These lists decide what a person is offered, and they are hand-written
    /// copies of a fact recorded elsewhere. A module that starts working and is
    /// still skipped is invisible; one that regresses and is still listed is a
    /// black screen with a Play button. Neither shows up in any other test, so
    /// this is the thing that keeps them honest — the same "regenerate and fail
    /// on drift" trade the vendored opcode tables make.
    /// The survey's own definition of a module worth offering somebody.
    fn plays_and_draws(full_lifecycle: bool, ink: u64) -> bool {
        full_lifecycle && ink > 0
    }

    #[test]
    fn the_skip_lists_match_the_survey_baseline() {
        let path = std::path::Path::new("../../tests/baseline/modules.json");
        let text = std::fs::read_to_string(path).expect("the baseline is committed");

        // A dependency-free read of the three things that matter. The file is
        // machine-written with one field per line, so this walks lines rather
        // than pulling in a JSON parser for a test.
        let mut excluded = BTreeSet::new();
        let (mut name, mut ink, mut plays) = (String::new(), 0u64, false);
        let mut seen = 0usize;
        for line in text.lines() {
            // Module blocks are nested one level deeper than the `"modules"` key
            // that holds them, which is the only thing distinguishing the two.
            let module_start = line
                .strip_prefix("  \"")
                .filter(|_| !line.starts_with("   "))
                .and_then(|rest| rest.strip_suffix("\": {"));
            let body = line.trim();
            if let Some(found) = module_start {
                // A new module block: bank the previous one first.
                if !name.is_empty() && !plays_and_draws(plays, ink) {
                    excluded.insert(name.clone());
                }
                name = found.to_owned();
                ink = 0;
                plays = false;
                seen = seen.saturating_add(1);
            } else if let Some(rest) = body.strip_prefix("\"ink\":") {
                ink = rest.trim().trim_end_matches(',').parse().unwrap_or(0);
            } else if let Some(rest) = body.strip_prefix("\"outcome\":") {
                plays = rest.contains("full lifecycle");
            }
        }
        if !name.is_empty() && !plays_and_draws(plays, ink) {
            excluded.insert(name);
        }

        // If the file's shape ever changes, this test must fail loudly rather
        // than quietly agreeing that nothing is excluded.
        assert!(
            seen > 90 && excluded.len() > 30,
            "parsed {seen} modules and {} exclusions; the baseline's shape changed",
            excluded.len()
        );

        let skipped: BTreeSet<String> = NEEDS_AFTER_DARK_3
            .iter()
            .chain(DOES_NOT_PLAY.iter())
            .map(|s| (*s).to_owned())
            .collect();

        let listed_but_broken: Vec<_> = excluded.difference(&skipped).collect();
        let skipped_but_working: Vec<_> = skipped.difference(&excluded).collect();
        assert!(
            listed_but_broken.is_empty(),
            "the survey says these do not play, but the launcher would list them: {listed_but_broken:?}"
        );
        assert!(
            skipped_but_working.is_empty(),
            "these are skipped but the survey says they play: {skipped_but_working:?}"
        );
    }

    /// The two lists do not overlap, so neither can be trimmed by accident on
    /// the belief that the other still covers a name.
    #[test]
    fn the_skip_lists_do_not_overlap() {
        for name in DOES_NOT_PLAY {
            assert!(
                !NEEDS_AFTER_DARK_3.contains(name),
                "{name} is in both skip lists"
            );
        }
    }
}
