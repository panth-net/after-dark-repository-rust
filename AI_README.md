# Technical notes

The detail that doesn't belong in a five-minute README: architecture, the
full CLI, packaging, testing methodology, and the reasoning behind decisions
that look arbitrary from the code alone. Read `README.md` first.

## Architecture

| crate | what it is |
|---|---|
| `ad-resource` | resource forks, module structure, settings, fonts. No dependencies, no panics on malformed input |
| `ad-m68k` | the 68000, via vendored Musashi with committed opcode tables |
| `ad-memory` | the emulated address space and Memory Manager heap |
| `ad-toolbox` | the Macintosh Toolbox: QuickDraw, Resource Manager, Sound Manager, SANE |
| `ad-host-v2` | the After Dark host ABI — module lifecycle and the `GMParamBlock` |
| `ad-runtime` | host policy: options, save locations, the audio mixer, PNG output. The only crate that reads the environment or knows what a path is |
| `ad-player` | the window: the browser and the interactive host |

## CLI reference

```sh
cargo run -p ad-player --release -- --screenshot ui.png   # draw the browser to a PNG and exit
cargo run -p ad-player --release -- --scores               # print the save folder and what's in it
cargo run -p ad-player --release -- --keytest               # plain grey window, prints every key pressed
```

`--keytest` opens no emulator and no module — it exists to isolate keyboard
problems. If keys show up there, input is working and the fault is in
module-specific handling; if nothing appears for twenty seconds, the input
event isn't reaching the process at all, and nothing in this codebase can fix
that. A mouse click also toggles Caps Lock in this window, since clicks and
key events arrive by different paths.

Environment variables:

```sh
AD_MHZ=16 cargo run -p ad-player --release   # override the emulated clock speed
AD_STATS=1 cargo run -p ad-player --release  # print achieved frame rate every second
```

Modules run at the pace of an emulated 8 MHz machine by default, held to the
wall clock. There's no single correct speed — After Dark shipped for
machines from an 8 MHz Mac Plus to a 40 MHz Quadra, and raising the clock
from 8 to 15.67 MHz changes ten of the 66 modules' behavior, in both
directions. `AD_MHZ` is exposed rather than hard-coded because of that.

The one exception is Lunatic Fringe, which runs at 40 MHz by default
regardless of `AD_MHZ`. It paces itself to 60 Hz internally, so a faster
clock doesn't speed it up — but at 8 MHz it only hits about half that rate,
and drops frames exactly when the screen fills with enemies, which reads as
the game bogging down mid-fight. A faster emulated clock keeps the busiest
scenes at the game's own cap.

`AD_STATS=1` exists because "it feels laggy" and "it's rendering 0.3 frames a
second" are the same bug report, and only the second one is actionable — that
number is what found a three-second sleep sitting in the redraw path.

## The first-run import

Launched with no arguments and no library, the player asks for a disk image
before it opens a window, extracts it, and never asks again.

* `crates/ad-resource/src/hfs.rs` reads the HFS volume — a port of
  `tools/audit/dump_all_forks.py`, verified byte-for-byte identical to it
  across all 108 forks on the 2.0 disk. The script stays for the lab and CI;
  the product cannot ask somebody to run Python before it will start.
* `crates/ad-runtime/src/library.rs` writes the result to `save_dir()/modules`
  — **outside** the `.app`, which is the whole point: the app can be moved,
  renamed, replaced or deleted and reinstalled without costing anybody their
  library. It stages into `modules.incoming` and renames on success, so a
  failure halfway through can't leave a half-library that looks importable.
  An import that yields modules but no font is refused up front, because the
  launcher draws its own chrome with a Macintosh strike and would otherwise
  come up as an unusable empty window.
* `crates/ad-player/src/setup.rs` is the conversation. It's a native dialog
  rather than something drawn in the window for a circular reason worth
  remembering: on first run the missing font *is* the thing being asked for,
  so there is nothing to draw a "you need a font" screen with.

Passing a path explicitly (`ad-player some/dir`) skips all of this and uses
what you named, failures included — that's the lab and dev path.

## Getting the original assets

`reference/` is a local, gitignored cache for the After Dark SDK and
programmer's manuals, so a checkout never has to re-download or re-extract
them. Nothing under it is published, and nothing under it is committed. **No
original material is in this repository at all** — not the modules, not the
fonts, not a disk image, and nothing inside the packaged app either. The
player imports them at run time from a disk the user supplies. To rebuild the
local cache:

- **SDK and programmer's manuals** (`reference/sdk/`) — `ADM3SD.SIT` (the AD
  3.0 Mac SDK: Think C, Metrowerks, MPW and Pascal examples), `PROGMA.SIT`
  (Mac programmer's manual), `PROGEX.SIT` (AD 2.0-era examples), `PROMAN.ZIP`
  (the AD 2.0 programmer's manual). Archived at
  [jamessignorile.com/program.html](http://jamessignorile.com/program.html).
  This is the primary specification for the module ABI — see
  `docs/LEARNINGS.md`.
- **`Old_World_Mac_Roms.zip`** (`reference/downloads/`) — the `MacIIci.ROM`,
  `Quadra650.ROM` and `Quadra800.ROM` the QEMU fidelity oracle boots, packaged
  with every other 68K/PowerPC Mac ROM. Archived at
  [Macintosh Repository](https://www.macintoshrepository.org/7038-all-macintosh-roms-68k-ppc-).
  Extract the three above into `reference/private/`.
- **`After_Dark_Classic.iso.zip`** (`reference/downloads/`) — the hybrid
  Win/Mac compilation CD holding Rat Race and 20 other 3.0-era modules.
  Archived at
  [Macintosh Repository](https://www.macintoshrepository.org/52936-after-dark-classic).
- **`AfterDark-original.img` and the rest of the After Dark family** — see the
  Internet Archive and Macintosh Garden links in the main README; extract
  with `tools/audit/dump_all_forks.py` as shown there. The 3.0-era releases
  hide their files inside StuffIt InstallerMaker installers rather than loose
  on the disk; the resource forks inside are ordinary, and extraction works
  once they're unpacked (`unar` opens all of it).
- **System 7.5.3 boot media for the QEMU fidelity oracle**
  (`reference/private/System7_5_3.img`) — Apple's own release, archived at
  [Internet Archive](https://archive.org/details/AppleMacintoshSystem753).
  `reference/private/System753.toast` is the same release in bootable-CD
  form. `AfterDark-apm.img` and `sys753-boot.img` aren't separate
  downloads — they're built locally from the images above by
  `tools/oracle/wrap_apm.py` and the driver-graft step in `tools/oracle/`.
- **The Lunatic Fringe resource fork test fixture**
  (`reference/lunatic-fringe/Lunatic Fringe.rsrc`) — extract it from
  `AfterDark-original.img` the same way as the other modules; the tests that
  use it (`crates/ad-resource/tests/lunatic_fringe.rs` and
  `rewrite_real_forks.rs`) skip with a message when it's absent, same as the
  CI compatibility survey does for the disk image itself.

## Lunatic Fringe control remapping

The original-layout column in the main README isn't a guess — it's read out
of the game's own `LFky 128` resource (fourteen records of
`[flags][character][key code]`), the same table the game draws on its own
help screen.

The modern layout is a remapping, not a change to the game: Lunatic Fringe
polls the `KeyMap` for exactly the codes in `LFky`, so `A` arrives as keypad
4. The game's key table stays truthful about what it expects. A remapped key
never also arrives as itself, which matters most for `A` — Turn Left in the
modern layout, Abort Ship in the original.

It is the layout the game **starts** in, because the original one is built
around a numeric keypad and most keyboards this runs on have none. The chooser
screen offers both side by side before the module starts, and that settles it
for the run — there is no mid-game swap and so no key held back from every
module to keep one available. The default is scoped to Lunatic
Fringe — keyed off the same `LFky` probe that decides the banner — because the
remapping targets codes from *that* module's key table; applying it to any
other module would quietly turn `A` into keypad 4 for no reason.

**Typing your name after a high score turns the remapping off.** It has to:
the game reads name entry from the same `KeyMap` it flies with and turns the
code into a character itself, so a remapped `A` would type "4" instead of
"a", and `Delete` (mapped to Abort Ship) would type "a" instead of erasing.
The player watches for the module's own prompt — *"High score!  Enter your
name:"* — drawn once, before the first keystroke, and while it's up, every
key sends its own code, `Delete` erases, and the three keys the player
otherwise reserves (`C`, `1`, `F`) are handed back so they can appear in a
name. A banner reads **TYPING** while this is active. Only `Esc` stays the
player's, since leaving has to work from every screen. Typing mode ends with
the `DrawFrame` that contained the prompt — for this game, that's the end of
the session, so a missed cue can't leave the next game unsteerable.

**Pause, start, and unpause are one control**, because that matches the
game's own help text ("Use Caps Lock to pause and unpause the game"). The
real key never arrives as a `keyDown:` on macOS — it produces only a
`flagsChanged:` event, which `minifb` handles for Ctrl/Shift/Alt/Cmd and no
further — so there used to be a stand-in, `1`, for that platform alone.

There isn't now. `ad_keystate::caps_lock` reads the physical lock as the flag
it is, so the real key drives the latch on macOS too, and Windows and Linux
have always delivered it as an ordinary event. Once the real key worked
everywhere, the substitute was a key held back from every module for nothing,
and it went back to being the digit `1`. A mouse click still flips the latch on
all three: clicks arrive through `mouseDown:` rather than `keyDown:`, so it is
the one control that survives a window receiving no keyboard events at all.
Starting a session with the lock already on counts as a change, which is what
somebody who switched it on before launching meant.

Pausing and switching to another application is safe: the latch moves on key
*events*, and an unfocused window receives none, so hitting Caps Lock
elsewhere can't unpause your game. On Windows and Linux that can leave the
keyboard light disagreeing with the game state — the title bar (**Paused** /
**Playing**) is the one to trust.

Verified end to end, not just reasoned about: driving the emulated Caps Lock
on, off, and on again puts the game into play, back to the home screen (shown
by the game itself, not this runtime), and into play again. Defaulting the
latch to *on* would be the obvious shortcut and is measurably wrong — it
breaks Life II (which correctly enters interactive mode and waits) and
Mountains.

Tab also flips the latch but shouldn't be relied on — macOS consumes Tab for
keyboard focus traversal before it reaches the window, so on this platform it
never arrives. `G`, `Return`, and `Tab` were all reserved at one point as
extra pause keys and have since been given back; they were scar tissue from a
stretch where it was unclear whether keyboard events were arriving at all,
and each failure added a key instead of removing the broken one. Tab can
never work here; `Return` is a key modules have a claim on; `G` was only ever
the key that happened to get verified first.

**The layout is chosen before the game starts, not defaulted.** Lunatic
Fringe's `LFky` declares *two* original key sets and polls both at once: the
keypad set (4/6 turn, 5 thrust, 8 turbo, 0 shield) and a keypad-free set
(`L`/`'` turn, `;` thrust, `P` turbo, Space shield), Command firing in either.
The chooser calls that layout **original-ish**, because two of its keys are
ours: `F` beside Command and `G` beside Space, the pair the keypad-free set
would otherwise require you to press as ⌘Space. See `ORIGINAL_MAP`.
The modern layout remaps `L` onto Turbo Thrust, so for anyone playing the
keypad-free set turning left silently stops existing — the key is read
correctly and lands on a control they were not aiming at. `;` is not remapped,
so thrust keeps working, which makes it look like one broken key rather than a
wrong layout.

Defaulting to modern and letting `C` fix it afterwards is what produced that,
so the choice is now asked once, up front, with both original sets drawn on
screen. Only a hand-started game asks; the idle timer and Preview are the
screen saver working with nobody watching, and a dialog waiting for Return
would stop it dead.

**Command fires, and holding it no longer breaks the other keys.** macOS
doesn't deliver `keyUp:` for an ordinary key while Command is held, so an input
layer built on the event stream loses the turn key's release and the ship keeps
turning after you let go. Command is Fire in this game, so that is the one
combination the original controls need most.

**`F` fires too, and on a laptop it is the one to use.** This is not a
preference: the game's own controls screen puts Power Shield on `Space` in its
keyboard column, so playing that column means holding Command and pressing
Space — Spotlight on any current Mac. System hot keys are dispatched by the
WindowServer before the application is offered the event, so no amount of input
handling can refuse it, and no version of this player will ever block it. The
keypad column has no such pair — Command with keypad `0` is not a system chord —
so the original controls are fine on a keyboard with a keypad and unplayable on
one without. `F` is what closes that gap, and it sidesteps the swallowed
`keyUp:` at the same time.

The fix is to stop asking the event stream. Every module steers by polling
the low-memory `KeyMap` — a bitmap of what is *physically down right now* —
and `ad-keystate` answers that question with `CGEventSourceKeyState`, which
reads the window server's HID-level state underneath event routing entirely.
It is the honest modern spelling of the `GetKeys` the module thinks it is
calling, and it cannot be affected by any way the event stream misreports.
Measured at 103 ns a call, so polling every key every frame costs about
0.05% of one core.

`F` and `J` remain as alternative Fire keys, for keyboards and habits that
prefer them, but they are no longer a workaround for anything.

The old timing heuristic — a key still held keeps auto-repeating, one whose
release was swallowed goes quiet — is still in the tree and still tested, but
it now runs only where the hardware read is unavailable: every platform but
macOS, and a macOS that hasn't yet answered. The two never run together; a
guess must not be allowed to veto a fact.

`--keytest` prints both views side by side (`win` is the event stream, `hw`
is the keyboard) and reports at the end whether they ever disagreed. Hold
Left, add Command, release Left: that is the divergence, and `hw` is the one
that stays right.

## Keys the player reserves

One is withheld from every module entirely:

| key | does |
|---|---|
| **Esc** | leave the module, on the second press |

One more is withheld from **Lunatic Fringe alone**, which is the pattern to copy
for anything like it:

| key | does | why only there |
|---|---|---|
| **1** | second pause key, beside Caps Lock | the pause banner is drawn for Lunatic Fringe and nothing else, so reserving it across the library would take a key from 140 modules to add a control to one. See `FRINGE_PAUSE` |

There were four across the board once. `C` swapped the control layout, which the
chooser screen now settles before the module starts; `M` muted, which the
browser's own **Sound** setting and the system volume both do better; `1` stood
in for a Caps Lock macOS would not deliver, which `ad_keystate` now reads
directly — it came back as a pause key scoped to the one game rather than a key
taken from all of them. Every key the player keeps is a key no module can see, so
each of those went back into `KEY_MAP` with its own code rather than merely
losing its binding.

Caps Lock is free by comparison: it is a system latch rather than a letter taken
from anyone, which is why it applies everywhere and `1` does not.

One is remapped — it still reaches the module, just as a different key:

| key | arrives as | why |
|---|---|---|
| **F** | Command (Fire) | a second Fire, so the keypad-free column is playable: its Power Shield is `Space`, and Command plus Space is Spotlight, which nothing can block. Also avoids Command dropping the *other* key's release |

## High score files

Saves are keyed on the module's **title**, not a hash of its bytes —
deliberately, since a hash would orphan every score the moment a module file
differed by one byte. The save folder is outside the application, so
deleting, replacing, or rebuilding the app doesn't touch scores. The files
are ordinary Macintosh resource forks holding the module's own saved
resources, so they're portable between platforms and between versions of
this runtime.

**Export** opens the platform's folder chooser and copies every save into
the folder you pick. **Import** opens a *file* chooser — pick the exported
`.rsrc` itself. Every imported file is parsed as a resource fork before
it's accepted; anything that isn't a save is refused by name rather than
copied and left to fail later. Import replaces a module's saved scores with
the ones in the file, and the report distinguishes replacements from
additions ("Imported 2, replaced 1 existing save(s)").

No file-dialog crate is involved — the platform's own chooser is invoked via
`osascript` on macOS, PowerShell on Windows, and `zenity` or `kdialog` on
Linux, which keeps this a two-dependency project. If none of those is
available, export reports that it couldn't ask, rather than guessing a
folder. The same logic is exposed without a dialog via `--export`/`--import`
on the command line, which matters on a machine with no folder chooser
installed and makes scripted backups possible.

## Screen saver mode

You can hand the idle slot to something other than a module. Add a `command`
line to `idle.conf` and the timer runs that instead, with the same delay and
the same "give the screen back the moment you touch the keyboard" behavior:

```
command = /path/to/an/emulator --with args
```

This exists for the After Dark 3.0-era modules — Rat Race and its neighbors
can't run in this runtime, because they go looking for the 3.0 engine on
disk via the File Manager, and there's no file system here (full diagnosis
in `docs/module-findings.md`). A real emulator with a real After Dark 3.0
install can run them today; this just gives it the same idle slot instead of
pretending otherwise. Point it at an executable rather than `open -a …`,
which detaches and leaves nothing to stop when you come back.

Settings are written to `idle.conf` beside your saved scores the moment you
click — a settings strip with no OK button that forgot itself would be worse
than no settings at all. It ships **off** by default; an application that
takes the screen the first time you run it is misbehaving.

Idle time comes from `IOHIDSystem`'s `HIDIdleTime`, the same counter the
window server's own idle timer reads — which is why it notices activity in
another application, not just this window. It's sampled once a second, not
once a frame, since reading it costs a subprocess.

When the timer fires (or Preview is clicked), the module runs in a
borderless window covering the display, dropped when the session ends.
`minifb` can't resize a window after creation, so this is a second window
created at screen size (measured via a shell-out to the Finder, the same
trade as the folder dialogs); if the display can't be measured, the ordinary
window floats to the front instead. It doesn't steal keyboard focus — click
to play, the same gesture that starts an interactive module normally.

**Why not a real `.saver` plug-in?** Since Catalina, macOS delivers no
keyboard events at all to third-party screen savers
([FB6916019](https://developer.apple.com/forums/thread/120901), open and
unanswered since 2019; [Aerial](https://github.com/JohnCoates/Aerial/issues/768)
hit the same wall), so Lunatic Fringe couldn't be played from one — controls
simply wouldn't arrive, and any input dismisses a saver regardless. Full
finding in `docs/LEARNINGS.md`.

This isn't as much of a gap as it sounds: the original After Dark launched
Lunatic Fringe from its "Sleep Now" corner with the module starting
paused — an idle-triggered session is what it always did. Because this is
the same application writing the same save directory, high scores from an
idle-started game land in the *same file* as ones from a game launched by
hand. A `.saver` runs sandboxed inside `legacyScreenSaver.appex`, where
writes are redirected into a container and would have forked scores in two.

## Building an app someone else can run

```sh
tools/package/make_app.sh
```

builds `dist/After Dark Player.app` and `dist/Lunatic Fringe Player.app` — the
second is the same binary with a module title in `Contents/Resources/module`, so
it opens the game rather than the list. Double-click either and it asks for a
disk image the first time. No terminal and no `cargo`. It ad-hoc signs the
result (`codesign -s -`, free, no Apple account — and not optional on Apple
Silicon, since a completely unsigned binary is killed by the kernel rather
than just warned about).

```sh
tools/package/make_dist.sh
```

is the same two applications for Linux and Windows, and runs on both — it works
out the `.exe` suffix from `uname`, so the Windows runner drives it under Git
Bash. There is no bundle off macOS, so the pin file sits *beside* the executable
instead of in `Contents/Resources`; `pinned_beside` reads both layouts and
`a_pinned_app_names_its_module_in_either_layout` holds it to both. Ship the
lookup without the packaging, or the packaging without the lookup, and every
Linux and Windows player gets the module list where the game should be.

**The bundle carries no modules and no fonts.** Those are Berkeley Systems'
and Apple's, and shipping them from a public repository is redistributing
someone else's software; an earlier version of this project did exactly that,
via a `!dist/**/*.rsrc` exception in `.gitignore` that undid the rule directly
above it. What used to be a build-time filter — bundle only the modules the
baseline records as completing their lifecycle and drawing ink — is now the
`DOES_NOT_PLAY` list in `crates/ad-player/src/library.rs`, applied when the
list is drawn. `the_skip_lists_match_the_survey_baseline` fails if that list
and `tests/baseline/modules.json` ever disagree, so the filter cannot rot.

`dist/` **is** committed — it's the download the README links to. That was a
problem when the bundle carried 55 modules and Apple's fonts; now it's a
universal binary and a plist, so the only cost is the binary itself. Re-run the
script and commit the result when you cut a new build.

To hand it to someone yourself instead of pointing them at the repo: zip it
first (email and messaging apps strip the executable bit, and an unzipped
`.app` arrives broken), then walk them through the right-click-Open dance
described in the main README.

## Code signing

Nothing here is signed with a paid certificate — no Apple Developer ID, no
Windows Authenticode. That's deliberate: it means a first-run warning on
macOS and Windows that has to be clicked through, and that gets documented
here instead of paid away.

**macOS.** The app *is* signed, ad-hoc (`codesign -s -`, free, no account
needed) — that part isn't optional, since Apple Silicon kills a completely
unsigned binary outright. What's missing is notarization, so Gatekeeper
refuses the first launch:

1. Open it. macOS says it can't verify the developer. Click **Done**, not
   "Move to Trash".
2. Open **System Settings → Privacy & Security**, scroll to the bottom.
3. There's a line about the app being blocked, with **Open Anyway**. Click
   it and authenticate.
4. Launch again and confirm once more. macOS remembers after that.

The terminal equivalent is `xattr -d com.apple.quarantine /path/to/app` —
faster if you already have one open.

A screen-saver `.saver` bundle is best-effort only here: "Open Anyway" is an
affordance for *applications*, and a plug-in that fails Gatekeeper is simply
not loaded, with no button to press. The standalone app is the primary
macOS artifact for that reason.

**Windows.** SmartScreen shows "Windows protected your PC" — click
**More info**, then **Run anyway**. To install the screen saver, right-click
the `.scr` and choose **Install**. Some browsers also warn on the download
itself and need "Keep anyway".

**Linux.** No signing to fight. `chmod +x` and run it.

## Testing and the compatibility matrix

`docs/compatibility-matrix.md`, regenerated by `tools/lab/matrix.py`, is the
authoritative per-module status — every column is a separate claim with its
own evidence, and `--` means **not evidenced**, never "passed." A module
isn't done just because it closed without an unhandled trap.

`tests/baseline/modules.json` is the regression gate: `tools/lab/survey.py`
compares against it and fails when any module gets worse.

Both are measured against the **`lab` profile** — optimized, with overflow
checks kept:

```sh
cargo build --profile lab --example run_module -p ad-host-v2
python3 tools/lab/survey.py --check tests/baseline/modules.json
```

That takes about three seconds for all 66 modules. A debug build once
reported a 240-second timeout on a module that actually finishes in 0.17s; a
plain release build would have missed an arithmetic overflow that was
silently drawing the wrong shape. The `lab` profile is the only one that
catches both.

Human testing has been macOS only. Windows and Linux builds are verified by
CI (they compile and the test suite passes) but nobody has watched them run.
