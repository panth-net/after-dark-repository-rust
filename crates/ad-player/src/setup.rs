//! First run: getting a module library onto the machine.
//!
//! The modules and the Macintosh fonts are Berkeley Systems' and Apple's, so
//! they are not shipped with this app. Somebody who has just double-clicked it
//! for the first time therefore has nothing to play, and this is the
//! conversation that fixes that — once, permanently, without a terminal.
//!
//! # Why the operating system's dialog and not the window
//!
//! The launcher draws its own interface with a real Macintosh font strike taken
//! out of the user's System file. On first run that file is precisely what is
//! missing, so there is no font to draw a "you need a font" screen with. A
//! native dialog needs nothing from us and is what the platform's own
//! first-run prompts look like anyway.

use std::path::PathBuf;

/// Where the original disk is archived. Linked rather than bundled: this is
/// Berkeley Systems' software, and the Internet Archive is where it is kept.
///
/// The **file** rather than its catalogue page, so the browser starts the
/// download instead of landing somebody on archive.org's sidebar of format
/// options to guess among. The page is one click away from there if they want
/// it, and the README links it for anybody reading that instead.
///
/// Verified against the copy this import was developed against: same 15,728,640
/// bytes, same md5 `eba358d5b1f7eef8bca754a1f54ccbbb`.
const DOWNLOAD_URL: &str = "https://archive.org/download/AfterDark_mac/AfterDark.img";

/// What the downloaded file is called, so the instructions can name it. Nobody
/// should have to work out which of several files is the right one.
const DOWNLOAD_FILE: &str = "AfterDark.img";

/// The dialogs speak for the application, so they carry its name.
const TITLE: &str = crate::APP_NAME;

/// Which panel of the conversation is on screen.
///
/// A panel is three buttons and a message, because `display dialog` allows no
/// more than three — so what the middle button *is* changes as the person moves
/// through, while Quit and Choose file… stay put on either side of it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// Nothing downloaded yet, so the middle button fetches it.
    Start,
    /// The download has started and its address is on screen, so the middle
    /// button copies that address.
    Downloading,
}

/// Make sure there is a library to browse, asking for one if there is not.
///
/// `None` means the person chose to quit, which is an outcome and not a
/// failure — the caller exits quietly rather than reporting an error.
pub fn ensure_library() -> Option<PathBuf> {
    let dir = ad_runtime::library_dir()?;
    if ad_runtime::have_library(&dir) {
        return Some(dir);
    }

    // Whether this is a genuine first run or a library that has gone missing
    // changes the first sentence, and nothing else. Someone whose modules
    // vanished should be told that, not greeted as a new arrival.
    // Both openings say what the button will *do* before it does it. A click
    // that silently starts a download reads as something that snuck one past
    // you, however much you wanted the file — so the sentence names the browser,
    // the file, its size, and who it comes from, and then nothing is a surprise.
    let mut message = if dir.exists() {
        // Their library went missing. They get the download button too: whatever
        // removed the modules may well have taken the original file with it.
        format!(
            "Your After Dark files have gone missing from\n{}\n\n\
             Choose your copy of {DOWNLOAD_FILE} if you still have it. Or click \
             Download After Dark, which opens your browser and downloads it from \
             the Internet Archive again (15 MB).",
            dir.display()
        )
    } else {
        format!(
            "After Dark was made by Berkeley Systems, so bring your own copy.\n\n\
             Clicking Download After Dark opens your browser and starts one \
             download from the Internet Archive: {DOWNLOAD_FILE} (15 MB).\n\n\
             You only do this once."
        )
    };

    let mut stage = Stage::Start;

    loop {
        let buttons: &[&str] = match stage {
            Stage::Start => &["Quit", "Download After Dark", "Choose file…"],
            // Once the download has started the address is on screen, so the
            // middle button becomes the one that gets it onto the clipboard —
            // for the person whose browser did not open, or who is downloading
            // it on their phone. Retyping that URL by hand is not an answer.
            Stage::Downloading => &["Quit", "Copy link", "Choose file…"],
        };
        let choice = ad_runtime::ask(TITLE, &message, buttons)?;
        let label = buttons.get(choice)?;

        match *label {
            "Quit" => return None,
            "Download After Dark" => {
                stage = Stage::Downloading;
                let lead = if ad_runtime::open_url(DOWNLOAD_URL) {
                    format!("Downloading {DOWNLOAD_FILE} (15 MB).")
                } else {
                    format!("Open this to download {DOWNLOAD_FILE} (15 MB):")
                };
                message = format!(
                    "{lead}\n\n{DOWNLOAD_URL}\n\n\
                     When it finishes, click Choose file… and pick {DOWNLOAD_FILE} \
                     from your Downloads folder.\n\n\
                     Then keep that file somewhere you won't delete it. \
                     Software this old doesn't always stay online."
                );
            }
            "Copy link" => {
                let lead = if ad_runtime::copy_to_clipboard(DOWNLOAD_URL) {
                    "Copied. Paste it into a browser:"
                } else {
                    "Couldn't reach the clipboard. Type this in:"
                };
                message = format!(
                    "{lead}\n\n{DOWNLOAD_URL}\n\n\
                     Then click Choose file… and pick {DOWNLOAD_FILE} from your \
                     Downloads folder."
                );
            }
            _ => {
                let Some(chosen) =
                    ad_runtime::choose_file_or_folder("Choose your After Dark download")
                else {
                    // Cancelled the file chooser: back to the same question,
                    // rather than quitting out from under them.
                    continue;
                };
                match ad_runtime::install_from(&chosen) {
                    Ok(count) => {
                        println!(
                            "Installed {} modules and {} fonts into {}",
                            count.modules,
                            count.fonts,
                            dir.display()
                        );
                        return Some(dir);
                    }
                    Err(why) => {
                        // A failed import keeps whichever panel they were on, so
                        // the link and its Copy button do not disappear at the
                        // moment somebody discovers they picked the wrong file.
                        message = format!("{why}\n\nTry a different file.");
                    }
                }
            }
        }
    }
}
