# After Dark Rust Player

The original 1991 After Dark screensaver modules, running on macOS, Windows, and Linux. 

After Dark is sadly abandonware: Berkeley Systems is gone and the original full After Dark suite is no longer commercially available. We grew up with the screensavers and wanted to set up a Rust implementation to emulate the original disks. We hope it's a helpful codebase to anyone who finds this repo.

![Four After Dark modules running: the Lunatic Fringe title screen, Mowin' Man,
Lunatic Fringe gameplay with its damage-control panel, and Flying
Toasters](frames.webp)

## Just want to play it?

Download the one for your machine:

- **[macOS](https://github.com/panth-net/after-dark-repository-rust/releases/latest/download/After-Dark-Player-macOS.zip)**
  — one download for both Apple Silicon and Intel
- **[Windows](https://github.com/panth-net/after-dark-repository-rust/releases/latest/download/After-Dark-Player-Windows-x64.zip)**
  — 64-bit
- **[Linux](https://github.com/panth-net/after-dark-repository-rust/releases/latest/download/After-Dark-Player-Linux-x86_64.tar.gz)**
  — x86_64

[Click here for all releases](https://github.com/panth-net/after-dark-repository-rust/releases)

Each download holds two applications:

- **After Dark Player** opens the list of screensavers.
- **Lunatic Fringe Player** (our favorite screensaver) opens Lunatic Fringe directly.

They're the same build underneath and share one library and one high score
table, so it doesn't matter which you launch.

The first launch asks for an original After Dark disk image
and offers to open the page to download it. [Download it (the ISO file)](https://archive.org/download/AfterDark_mac/AfterDark.img), then select it in the file dialog.

## Running the app

**macOS** — unzip, drag both applications into Applications, then right-click
one and choose **Open**. macOS will warn that the developer isn't verified;
click **Open** anyway. You only need to do this once. 

If you are not able to open it, then visit
*System Settings → Privacy & Security*, where the block appears as a button to
`Open Anyway`. Click that, and the app will now launch.

[![System Settings → Privacy & Security, with the blocked-application notice and
its "Open Anyway" button](image.webp)](image.webp)


**Windows** — unzip anywhere and open the **After Dark Player** folder, then run
**After Dark Player.exe**. SmartScreen will say the publisher is unknown: click
**More info**, then **Run anyway**. Once only.

**Linux** — extract it and run the binary:

```sh
tar -xzf After-Dark-Player-Linux-x86_64.tar.gz
"./After Dark Player/After Dark Player"
```

It wants ALSA for sound (`libasound2`, already there on most desktops) and
either X11 or Wayland, whichever you're running. The build comes off the current
Ubuntu runner, so a much older distribution may find its glibc too old — build
from source in that case, which is a few lines further down.

**The app fetches the original After Dark image for you**, but
if you'd rather do it yourself it's one 15 MB file:
[AfterDark.img](https://archive.org/download/AfterDark_mac/AfterDark.img), from
[this Internet Archive page](https://archive.org/details/AfterDark_mac), where
it's filed under "ISO IMAGE."
[Macintosh Garden](https://macintoshgarden.org/apps/after-dark-3) has it too.


**Why the hoops?** An Apple
Developer Program membership at $99/year, or a Windows code-signing certificate
at a few hundred a year. Since this is a side project, please excuse our team in not
purchasing them.

## Shoutouts to the After Dark community

We started this project because we wanted to play the original *Lunatic Fringe* — and eventually the rest of *After Dark* — directly on a modern desktop. We're newcomers to the After Dark online community and it was amazing to see all of the work that already exists, all really fantastic projects, so we want to credit all of them as well as the original authors of After Dark.

Our repo is just our open source contribution, and we hope it helps anyone else who wants to build more with Rust.

Note: *this repo is an independent project and is not affiliated with or endorsed by any of the projects below.*

This is not a complete list so please excuse it if it misses folks, but we wanted to shoutout as many as possible:

**[Flying Toasters at mass:werk](https://www.masswerk.at/flyer/)** by Norbert Landsteiner — a browser recreation of the classic module.

**[After Dark for OS 9](https://www.macintoshrepository.org/1859-after-dark-for-os-9)** — an earlier patched version of the After Dark 4.0 engine, attributed by later preservation work to Daxeria, that kept it running on Mac OS 9.

**[Fringe Player](http://www.sealiesoftware.com/fringe/)** by Greg Parker and Sealie Software — first through the Mac OS X Classic environment and later as a native PowerPC and Intel application, running the original *Lunatic Fringe* module.

**[The After Dark Screensaver archive](https://afterdarksaver.blogspot.com/)** by David Donarumo — documentation, compatibility notes, module history, and resources for keeping the Windows versions of After Dark running on newer systems.

**[Flying Toasters 3.1.0 for OS X](https://www.macintoshrepository.org/885-flying-toasters-3-1-0-for-osx)** by Heiko Kretschmer — a Universal Binary screensaver that brought Flying Toasters to PowerPC and Intel Macs.

**[Starryn](https://github.com/evangreen/starryn)** by Evan Green — a Windows recreation of the original *Starry Night* module.

**[Lunatic Fringe for the web](https://github.com/jackinloadup/lunatic-fringe)** — a browser recreation begun by James Carnley, substantially developed by schwal10, and later continued by jackinloadup.

**[After Dark in CSS](https://github.com/bryanbraun/after-dark-css)** by Bryan Braun — browser recreations of several classic modules made with CSS animations and transforms.

**[M.A.C.E.](https://mace.home.blog/2019/04/08/experimenting-with-init-cdev-support/)** — a broader classic Macintosh compatibility environment that implemented enough of the old Macintosh system APIs to run the original After Dark.

**[After Dark X and the After Dark Classic Set](https://en.infinisys.co.jp/download/index.shtml)** by Infinisys — commercial Mac OS X revivals of selected After Dark modules.

[Other modern Flying Toasters ports](https://github.com/robertventurini/FlyingToasters), including [Robert Venturini’s macOS screensaver](https://github.com/robertventurini/FlyingToasters), [BeaVix’s browser version](https://github.com/BeaVix/FlyingToastersJS), [torunar’s XScreenSaver version](https://github.com/torunar/flying-toasters-xscreensaver), and [Marcus Greenwood’s Wayland/X11 version](https://github.com/marcusgreenwood/flying-toasters-wayland).

**[After Dark 4.94 Collection](https://www.macintoshrepository.org/84473-after-dark-4-94-collection)** by CybernetixZero — a patched and consolidated installation to run on Mac OS 9.2.2, building on Daxeria’s earlier compatibility work.

**[Ode to the Flying Toaster](https://flyingtoasters.greggant.com/)** by Greg Gant — a modern macOS homage with builds for both current Macs and older PowerPC-era versions of OS X.

**[Lunacy](https://morphing.cloud/lunacy/)** by Jeff Halter — a modern macOS player that runs the original *Lunatic Fringe* module.

Also the [Internet Archive](https://archive.org/details/AfterDark_mac), [Macintosh Garden](https://macintoshgarden.org/apps/after-dark-3), [Macintosh Repository](https://www.macintoshrepository.org/), and [WinWorld](https://winworldpc.com/product/after-dark) that preserve the original disks and community knowledge.

And credit to Berkeley Systems and the original module authors — especially Ben Haller, who created *Lunatic Fringe*. 

___



## Running from source

To run it yourself from source, you need [Rust](https://rustup.rs).

```sh
git clone https://github.com/panth-net/after-dark-repository-rust.git
cd after-dark-repository-rust
cargo run -p ad-player --release
```

On first run it asks for your disk image, same as the packaged app, and keeps
what it extracts in the platform's application-support directory. After that it
opens a window listing the modules. Arrow keys select, **Return** plays the
highlighted one, **Esc** leaves a running module, **Esc** again quits. Always
use `--release` — a debug build runs roughly 10x slower.

![The module list window: 76 modules down the left, the selected module's
resource count and settings on the right, and the screen-saver idle bar along
the bottom](menu.webp)

To extract by hand instead — into `./modules`, which the player uses when you
pass it a path:

```sh
python3 tools/audit/dump_all_forks.py AfterDark-original.img modules
cargo run -p ad-player --release -- modules
```

More sources, including the SDK and ROM images used for testing, are listed in
[docs/technical-notes.md](docs/technical-notes.md#getting-the-original-assets).

A few other useful commands:

```sh
cargo run -p ad-player --release -- ~/Modules                      # browse a different folder
cargo run -p ad-player --release -- "modules/Flying Toasters.rsrc" # run one module directly
cargo run -p ad-player --release -- --export <folder>              # back up your high scores
cargo run -p ad-player --release -- --import <folder-or-file>      # restore them
```

## Playing Lunatic Fringe

The one module with real controls, not just visuals. Starting it asks which
layout you want — **Up/Down** chooses, **Return** starts, **Esc** goes back.
Both layouts are shown side by side, so you pick once and play; there is
nothing to swap mid-game and nothing to remember.

**Caps Lock** starts and pauses the game, which is what the game's own screen
asks for — or click the window, if you'd rather. **Esc** twice goes back to the
list. Those are the only keys the player keeps; everything else on the keyboard
goes to the module.

## High scores

Saved automatically, alongside your imported modules, to:

- macOS: `~/Library/Application Support/After Dark/`
- Windows: `%APPDATA%\After Dark\`
- Linux: `$XDG_DATA_HOME/after-dark/`

Nothing lives inside the app itself, so replacing or deleting it costs you
neither your scores nor your modules.

Use the **Export**/**Import** buttons (or `E`/`I`) in the module list to back
them up or move them to another machine.

## Screen saver mode

The bar along the bottom of the module list turns on idle-timeout behavior:
pick a module (or Random), set a delay, and it takes over the screen after
you've been away. This is the same application taking over the screen, not an
OS-level `.saver` plug-in — recent macOS versions block keyboard input to
those, which would make Lunatic Fringe unplayable that way. Details in
[docs/technical-notes.md](docs/technical-notes.md#screen-saver-mode).

## What works

[docs/compatibility-matrix.md](docs/compatibility-matrix.md) has the
module-by-module status. Development and hands-on testing were done on macOS.
All three platforms build and pass the test suite in CI; the Linux and Windows
builds have had less time on real hardware, so if something looks wrong there,
it may well be.

## More detail

- [docs/technical-notes.md](docs/technical-notes.md) — build layout, packaging,
  testing, and the reasoning behind decisions that look arbitrary from the code
  alone.
- [docs/compatibility-matrix.md](docs/compatibility-matrix.md) — per-module
  status, kept current by tooling.

## Status

Not actively maintained. This was a one-time build. Issues and
pull requests may sit unread. If you want to take it over, [contact us here](https://www.pantheonnetwork.co/contact) and
say so. We're happy to hand it off.

## License

The Rust code is MIT — see [LICENSE](LICENSE).

Nothing else here is ours and none of it ships with this project. The After
Dark modules, artwork and sounds are Berkeley Systems'; the Macintosh fonts are
Apple's. 

Both come off the disk you supply and stay on your machine. The
vendored 68k core under `crates/ad-m68k/vendor/` is Karl Stenerud's, under MIT
— its own notices are in that folder.
