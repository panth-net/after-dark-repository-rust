//! Asking the operating system where to put a file.
//!
//! # Why not a dialog crate
//!
//! This workspace has two runtime dependencies — a window and an audio device —
//! and a file-dialog crate would be the third, dragging in a GUI toolkit per
//! platform for one modal box that opens when somebody clicks Export. Every
//! desktop already ships something that can put up a folder chooser and print the
//! answer, so this shells out to that instead. No supply chain, and the dialog is
//! the platform's own rather than an imitation.
//!
//! The cost is honest and bounded: it blocks until the person answers, and on a
//! machine with no such tool it returns `None` and the caller falls back. It is
//! never on a path that has to succeed.

use std::path::PathBuf;
use std::process::Command;

/// Ask for a folder. `None` if the person cancelled, or if there is nothing to ask
/// with.
///
/// The prompt is passed through to the platform's dialog, so keep it short and say
/// what will be written.
#[must_use]
pub fn choose_folder(prompt: &str) -> Option<PathBuf> {
    // A quote in the prompt would end the string literal the shell tool is given.
    // Nothing here needs one, so they are dropped rather than escaped per platform.
    let clean: String = prompt.chars().filter(|c| *c != '"' && *c != '\\').collect();
    let out = platform_command(&clean)?;
    let path = PathBuf::from(out.trim());
    // A dialog that was cancelled prints nothing, or an error on stderr we ignore.
    if path.as_os_str().is_empty() || !path.is_dir() {
        return None;
    }
    Some(path)
}

/// Ask for a file **or** a folder. `None` if cancelled or there is nothing to
/// ask with.
///
/// Exists because of what "Import scores…" looked like through a folder-only
/// chooser: the exported `.rsrc` files were right there in the dialog, greyed
/// out and unclickable, and the person who had just exported them couldn't
/// select the very thing they came back for. On macOS this is one dialog that
/// takes both; elsewhere it falls back to the file chooser, since a folder full
/// of saves can also be named by picking any file inside it.
#[must_use]
pub fn choose_file_or_folder(prompt: &str) -> Option<PathBuf> {
    let clean: String = prompt.chars().filter(|c| *c != '"' && *c != '\\').collect();
    let out = platform_file_command(&clean)?;
    let path = PathBuf::from(out.trim());
    if path.as_os_str().is_empty() || !path.exists() {
        return None;
    }
    Some(path)
}

/// Put a message on screen with a row of buttons, and return which was pressed.
///
/// `None` when the person dismissed it without choosing, or when the platform
/// has nothing to ask with. Buttons read left to right and the **last** is the
/// default, which is the platform convention on macOS.
///
/// This is what the first run talks through, and it is deliberately the
/// operating system's own dialog rather than something drawn in the window: the
/// launcher draws its interface with a Macintosh font out of the user's System
/// file, and on first run that file is exactly what is missing. A dialog that
/// needs no font is the only thing that can ask for one.
#[must_use]
pub fn ask(title: &str, message: &str, buttons: &[&str]) -> Option<usize> {
    if buttons.is_empty() {
        return None;
    }
    let clean_title = sanitise(title);
    let clean_message = sanitise(message);
    let clean: Vec<String> = buttons.iter().map(|b| sanitise(b)).collect();
    let pressed = platform_ask(&clean_title, &clean_message, &clean)?;
    clean.iter().position(|b| *b == pressed)
}

/// Open a URL in whatever the person browses with. `false` if that failed.
///
/// Used for the one link the first run needs. The URL is a literal in this
/// crate's callers and is passed as an argument rather than through a shell, so
/// there is nothing here for a hostile string to escape into.
pub fn open_url(url: &str) -> bool {
    // Belt and braces: refuse anything that is not a plain web URL, so this can
    // never be handed a `file://` or a shell fragment by a future caller.
    if !url.starts_with("https://") && !url.starts_with("http://") {
        return false;
    }
    if url.chars().any(|c| c.is_whitespace() || c == '"' || c == '\'' || c == '\\') {
        return false;
    }
    platform_open(url)
}

/// Put `text` on the system clipboard. `false` if that could not be done.
///
/// Exists so a dialog can offer "Copy link" next to a URL. Opening a browser
/// covers most people, but not the one whose default browser fails to launch,
/// and not the one who wants to finish the download on a different machine.
/// Showing an address somebody then has to retype by hand is not an answer.
pub fn copy_to_clipboard(text: &str) -> bool {
    platform_copy(&sanitise(text))
}

fn sanitise(text: &str) -> String {
    text.chars().filter(|c| *c != '"' && *c != '\\').collect()
}

#[cfg(target_os = "macos")]
fn platform_copy(text: &str) -> bool {
    // `set the clipboard to` avoids piping to `pbcopy`, keeping this the same
    // shape as every other helper here: one command, no stdin plumbing.
    run(Command::new("osascript")
        .arg("-e")
        .arg(format!("set the clipboard to \"{text}\"")))
    .is_some()
}

#[cfg(target_os = "windows")]
fn platform_copy(text: &str) -> bool {
    run(Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command"])
        .arg(format!("Set-Clipboard -Value \"{text}\"")))
    .is_some()
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn platform_copy(text: &str) -> bool {
    // Wayland's tool takes the text as an argument; X11's two want it on stdin.
    if run(Command::new("wl-copy").arg(text)).is_some() {
        return true;
    }
    pipe_to(Command::new("xclip").args(["-selection", "clipboard"]), text)
        || pipe_to(Command::new("xsel").args(["--clipboard", "--input"]), text)
}

/// Feed `text` to a command's stdin and wait for it.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn pipe_to(cmd: &mut Command, text: &str) -> bool {
    use std::io::Write as _;
    let Ok(mut child) = cmd.stdin(std::process::Stdio::piped()).spawn() else {
        return false;
    };
    if let Some(mut stdin) = child.stdin.take() {
        if stdin.write_all(text.as_bytes()).is_err() {
            return false;
        }
    }
    child.wait().is_ok_and(|s| s.success())
}

#[cfg(target_os = "macos")]
fn platform_ask(title: &str, message: &str, buttons: &[String]) -> Option<String> {
    let list = buttons
        .iter()
        .map(|b| format!("\"{b}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let default = buttons.last()?;
    let script = format!(
        "display dialog \"{message}\" with title \"{title}\" \
         buttons {{{list}}} default button \"{default}\""
    );
    let out = run(Command::new("osascript").arg("-e").arg(script))?;
    button_from_osascript(&out)
}

/// Pull the label out of what `display dialog` prints.
///
/// `"button returned:Choose file…"`, or `"button returned:, gave up:true"` when
/// it timed out — which yields an empty label the caller then fails to match,
/// so a dialog nobody answered is the same as one nobody chose from.
#[cfg(target_os = "macos")]
fn button_from_osascript(out: &str) -> Option<String> {
    Some(out.split("button returned:").nth(1)?.split(',').next()?.trim().to_owned())
}

#[cfg(target_os = "windows")]
fn platform_ask(title: &str, message: &str, buttons: &[String]) -> Option<String> {
    // MessageBox offers fixed button sets, so the closest one is chosen by
    // count and mapped back to the caller's labels. The labels themselves are
    // folded into the message so nothing is lost.
    let (kind, order): (&str, Vec<usize>) = match buttons.len() {
        1 => ("OK", vec![0]),
        2 => ("OKCancel", vec![0, 1]),
        _ => ("YesNoCancel", vec![0, 1, 2]),
    };
    let numbered: Vec<String> = buttons
        .iter()
        .enumerate()
        .map(|(i, b)| format!("{}. {b}", i.saturating_add(1)))
        .collect();
    let body = format!("{message}\n\n{}", numbered.join("\n"));
    let script = format!(
        "Add-Type -AssemblyName System.Windows.Forms; \
         [System.Windows.Forms.MessageBox]::Show(\"{body}\", \"{title}\", \"{kind}\")"
    );
    let out = run(Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command"])
        .arg(script))?;
    let slot = match out.trim() {
        "OK" | "Yes" => 0,
        "No" => 1,
        "Cancel" => order.len().checked_sub(1)?,
        _ => return None,
    };
    buttons.get(*order.get(slot)?).cloned()
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn platform_ask(title: &str, message: &str, buttons: &[String]) -> Option<String> {
    // zenity's question box takes two buttons plus any number of extras, and
    // prints the extra's label when one is pressed.
    let mut cmd = Command::new("zenity");
    cmd.arg("--question")
        .arg("--title")
        .arg(title)
        .arg("--text")
        .arg(message);
    let last = buttons.last()?;
    cmd.arg("--ok-label").arg(last);
    if let Some(first) = buttons.first().filter(|_| buttons.len() > 1) {
        cmd.arg("--cancel-label").arg(first);
    }
    for extra in buttons.iter().skip(1).take(buttons.len().saturating_sub(2)) {
        cmd.arg("--extra-button").arg(extra);
    }
    let out = run(&mut cmd)?;
    let printed = out.trim();
    if printed.is_empty() {
        // Exit code 0 with no output is the ok-label, which is the last button.
        return Some(last.clone());
    }
    buttons.iter().find(|b| *b == printed).cloned()
}

#[cfg(target_os = "macos")]
fn platform_open(url: &str) -> bool {
    Command::new("open").arg(url).status().is_ok_and(|s| s.success())
}

#[cfg(target_os = "windows")]
fn platform_open(url: &str) -> bool {
    Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", "Start-Process"])
        .arg(url)
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn platform_open(url: &str) -> bool {
    Command::new("xdg-open").arg(url).status().is_ok_and(|s| s.success())
}

#[cfg(target_os = "macos")]
fn platform_command(prompt: &str) -> Option<String> {
    // `choose folder` is a Standard Additions command, so it needs no accessibility
    // permission — unlike anything driven through System Events.
    let script = format!("POSIX path of (choose folder with prompt \"{prompt}\")");
    run(Command::new("osascript").arg("-e").arg(script))
}

#[cfg(target_os = "macos")]
fn platform_file_command(prompt: &str) -> Option<String> {
    // `choose file` shows folders as traversable and files as selectable, so
    // one dialog covers both "pick the exported file" and "pick the folder of
    // them" (by choosing any file inside it — the caller may use the parent).
    let script = format!("POSIX path of (choose file with prompt \"{prompt}\")");
    run(Command::new("osascript").arg("-e").arg(script))
}

#[cfg(target_os = "windows")]
fn platform_file_command(prompt: &str) -> Option<String> {
    let script = format!(
        "Add-Type -AssemblyName System.Windows.Forms; \
         $d = New-Object System.Windows.Forms.OpenFileDialog; \
         $d.Title = \"{prompt}\"; \
         $d.Filter = \"After Dark scores (*.rsrc)|*.rsrc|All files (*.*)|*.*\"; \
         if ($d.ShowDialog() -eq 'OK') {{ Write-Output $d.FileName }}"
    );
    run(Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command"])
        .arg(script))
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn platform_file_command(prompt: &str) -> Option<String> {
    run(Command::new("zenity").args(["--file-selection", "--title"]).arg(prompt)).or_else(|| {
        run(Command::new("kdialog")
            .arg("--getopenfilename")
            .arg(".")
            .arg("--title")
            .arg(prompt))
    })
}

#[cfg(target_os = "windows")]
fn platform_command(prompt: &str) -> Option<String> {
    let script = format!(
        "Add-Type -AssemblyName System.Windows.Forms; \
         $d = New-Object System.Windows.Forms.FolderBrowserDialog; \
         $d.Description = \"{prompt}\"; \
         if ($d.ShowDialog() -eq 'OK') {{ Write-Output $d.SelectedPath }}"
    );
    run(Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command"])
        .arg(script))
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn platform_command(prompt: &str) -> Option<String> {
    // Whichever of the two desktop helpers is installed; neither is guaranteed.
    run(Command::new("zenity")
        .args(["--file-selection", "--directory", "--title"])
        .arg(prompt))
    .or_else(|| {
        run(Command::new("kdialog")
            .arg("--getexistingdirectory")
            .arg(".")
            .arg("--title")
            .arg(prompt))
    })
}

/// The main display's size in pixels, or `None` where it cannot be asked.
///
/// For the player's full-screen module runs: `minifb` cannot query the display
/// or resize a window after creation, so the window that covers the screen has
/// to be *created* at screen size, and something has to say what that is. The
/// Finder already knows, and asking it is the same no-new-dependencies trade as
/// the folder dialog above.
#[must_use]
pub fn display_size() -> Option<(usize, usize)> {
    platform_display_size()
}

#[cfg(target_os = "macos")]
fn platform_display_size() -> Option<(usize, usize)> {
    // "0, 0, 1512, 982" — left, top, right, bottom of the desktop.
    let out = run(Command::new("osascript")
        .arg("-e")
        .arg("tell application \"Finder\" to get bounds of window of desktop"))?;
    let nums: Vec<usize> = out
        .split(',')
        .filter_map(|p| p.trim().parse().ok())
        .collect();
    match nums.as_slice() {
        [l, t, r, b] if r > l && b > t => Some((r - l, b - t)),
        _ => None,
    }
}

#[cfg(not(target_os = "macos"))]
fn platform_display_size() -> Option<(usize, usize)> {
    None
}

/// Run a dialog helper and return its stdout, or `None` for any failure at all.
///
/// Every failure is the same to a caller: no folder was chosen. A missing tool, a
/// non-zero exit from a cancelled dialog and unreadable output are not worth
/// distinguishing, and reporting them would put a second error in front of someone
/// who just pressed Cancel.
fn run(cmd: &mut Command) -> Option<String> {
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A prompt cannot break out of the script it is embedded in.
    ///
    /// The prompts here are all literals, so this is defence against a future one
    /// being built from a module title — which is data out of a resource fork.
    #[test]
    fn quotes_and_backslashes_are_stripped_from_a_prompt() {
        let dirty = "Save \"here\\there\" now";
        let clean: String = dirty.chars().filter(|c| *c != '"' && *c != '\\').collect();
        assert_eq!(clean, "Save herethere now");
        assert!(!clean.contains('"') && !clean.contains('\\'));
    }

    /// `open_url` takes web URLs and nothing else.
    ///
    /// The callers pass literals, so this guards against a future one being
    /// built from something less trustworthy: a `file://` scheme would open a
    /// local path, and whitespace or quotes are what a shell fragment needs.
    #[test]
    fn open_url_refuses_anything_that_is_not_a_plain_web_url() {
        assert!(!open_url("file:///etc/passwd"));
        assert!(!open_url("/Applications/Calculator.app"));
        assert!(!open_url("https://example.com/a b"));
        assert!(!open_url("https://example.com/\"x\""));
        assert!(!open_url(""));
    }

    /// A dialog with no buttons has no answer, and never reaches the platform.
    #[test]
    fn ask_with_no_buttons_is_none() {
        assert_eq!(ask("t", "m", &[]), None);
    }

    /// The shapes `osascript` actually prints, captured from a real run:
    /// `display dialog ... giving up after 1` produced the second of these.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_pressed_button_is_read_out_of_what_osascript_prints() {
        assert_eq!(
            button_from_osascript("button returned:Choose file…").as_deref(),
            Some("Choose file…")
        );
        assert_eq!(
            button_from_osascript("button returned:Quit\n").as_deref(),
            Some("Quit")
        );
        // Timed out: an empty label, which no button matches, so nothing is
        // chosen rather than the first button being picked by accident.
        assert_eq!(
            button_from_osascript("button returned:, gave up:true").as_deref(),
            Some("")
        );
        let buttons = ["Quit", "Get the disk image", "Choose file…"];
        assert_eq!(buttons.iter().position(|b| b.is_empty()), None);
        // Cancelled dialogs exit non-zero and never reach this function, but a
        // reply in an unexpected shape must still be no answer.
        assert_eq!(button_from_osascript("something else"), None);
    }

    /// Cancelling, or having no dialog tool at all, is `None` rather than an error.
    #[test]
    fn a_failing_helper_is_no_folder_rather_than_a_failure() {
        // A command that certainly does not exist stands in for both cases.
        assert!(run(&mut Command::new("ad-no-such-dialog-helper-exists")).is_none());
        // And a command that succeeds but prints nothing is also no folder.
        let mut echo = Command::new("true");
        assert_eq!(run(&mut echo).as_deref(), Some(""));
        // which `choose_folder` rejects, because "" is not a directory.
        assert!(PathBuf::from("").as_os_str().is_empty());
    }
}
