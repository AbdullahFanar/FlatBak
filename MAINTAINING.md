# Maintaining FlatBak

Reference for anyone maintaining or contributing to FlatBak, in rough order of
how often each topic comes up.

- [Project structure](#project-structure)
- [Identity: the application ID](#identity-the-application-id)
- [The logo](#the-logo)
- [Changing the wording](#changing-the-wording)
- [Licensing](#licensing)
- [Making a release](#making-a-release)
- [The compatibility contract](#the-compatibility-contract)
- [Things that break quietly](#things-that-break-quietly)
- [Development workflow](#development-workflow)
- [Distribution](#distribution)
- [What FlatBak writes on a user's machine](#what-flatbak-writes-on-a-users-machine)

---

## Project structure

```
FlatBak/
├── Cargo.toml               Package metadata, dependencies, release profile
├── Cargo.lock               Committed: pins exact dependency versions
├── Makefile                 build / test / clippy / install / uninstall
├── io.github.abdullahfanar.FlatBak.json   Flatpak manifest
├── generated-sources.json   Vendored Cargo sources for the Flatpak build
├── LICENSE                  MIT
├── README.md                User- and contributor-facing overview
├── MAINTAINING.md           This file
│
├── src/
│   ├── main.rs              Binary: GtkApplication setup, `flatbak file.flatbak`
│   ├── lib.rs               Library root — everything else lives here
│   │
│   ├── config.rs            APP_ID, version, format version constants
│   ├── error.rs             Error types callers must distinguish, plus `Issue`
│   ├── util.rs              Size formatting, path safety, cancellation flag
│   ├── appdata.rs           Reading and measuring ~/.var/app/<APP_ID>
│   ├── recent.rs            The "Recent Backups" list on disk
│   │
│   ├── flatpak/
│   │   ├── mod.rs
│   │   ├── model.rs         InstalledApp, Remote, Installation
│   │   └── cli.rs           Drives the `flatpak` command; host-escape detection
│   │
│   ├── backup/
│   │   ├── mod.rs           Timestamp helpers, PAYLOAD_DATA_DIR
│   │   ├── container.rs     The .flatbak binary framing (header/trailer)
│   │   ├── manifest.rs      The versioned TOML manifest and its validation
│   │   ├── writer.rs        Creating an archive
│   │   └── reader.rs        Opening, validating and restoring an archive
│   │
│   └── ui/
│       ├── mod.rs           Module wiring, icon search paths
│       ├── app.rs           The window, shared state, window actions
│       ├── home.rs          Home page and the recent backups list
│       ├── backup_page.rs   Select → destination → review → run → result
│       ├── restore_page.rs  Open → resolve → run → result
│       ├── progress.rs      Reusable progress and result pages
│       ├── widgets.rs       Shared widget builders, the details log
│       └── dialogs.rs       Alerts, file choosers, About
│
├── data/                    Everything installed outside the binary
│   ├── io.github.abdullahfanar.FlatBak.desktop        Desktop entry
│   ├── io.github.abdullahfanar.FlatBak.svg            Application icon
│   ├── io.github.abdullahfanar.FlatBak.metainfo.xml   AppStream metadata
│   └── io.github.abdullahfanar.FlatBak.xml            MIME type for .flatbak
│
├── tests/
│   ├── roundtrip.rs         Backup → restore against a scratch data root
│   └── archive_safety.rs    Hand-built damaged and hostile archives
│
├── examples/                Development helpers, not shipped
│   ├── probe.rs             What FlatBak sees of this machine's Flatpak state
│   ├── selftest.rs          Real backup/restore cycle, compared byte for byte
│   └── render_ui.rs         Renders every page to PNG
│
└── tools/
    └── render-ui.sh         Runs render_ui inside a headless Weston
```

The split between `lib.rs` and `main.rs` exists so the integration tests in
`tests/` can drive the real backup and restore code. Keep new logic in the
library; `main.rs` should stay a thin shell.

`src/ui/` is the only part that needs a display. Everything else is testable
headlessly, and should stay that way.

---

## Identity: the application ID

```
io.github.abdullahfanar.FlatBak
```

This is a reverse-DNS name, and the rule is that it must correspond to a domain
the project actually controls. FlatBak is hosted at
`github.com/AbdullahFanar/FlatBak`, so the ID derives from the project's GitHub
Pages domain, `abdullahfanar.github.io`. Flathub verifies this. An ID such as
`io.github.flatbak.FlatBak` would claim an unrelated domain and be rejected.

If the repository ever moves to a different account or a real domain, the ID
has to move with it — see the warning below about what that costs.

The ID appears in six places, and they must all agree:

| Where | What it is |
| --- | --- |
| `src/config.rs` → `APP_ID` | The single source of truth in code |
| `Makefile` → `APP_ID` | Drives the install paths |
| `data/<ID>.desktop` | Filename, plus `Icon=` inside it |
| `data/<ID>.svg` | Filename — this is how the icon is found |
| `data/<ID>.metainfo.xml` | Filename, plus `<id>` and `<launchable>` inside |
| `data/<ID>.xml` | Filename (the MIME definition) |

It is also the D-Bus name the application registers and the key GNOME Shell
uses to match a running window to its `.desktop` file. **Renaming it after a
release breaks window matching, pinned launchers and any per-application
settings for everyone who already installed it**, so treat it as permanent
once published.

`appstreamcli` emits a *pedantic* note that the ID contains uppercase letters.
That is fine and intentional — Flathub is full of apps like
`com.github.tchx84.Flatseal` — and it is not a validation failure.

---

## The logo

The source is a single hand-written SVG:

```
data/io.github.abdullahfanar.FlatBak.svg
```

It is a 128×128 `viewBox` drawing an archive box with a restore arrow, using
GNOME's blue palette (`#62a0ea` → `#1c71d8`). There is no build step; the file
is installed as-is.

### How it gets found

```sh
make install    # installs to $PREFIX/share/icons/hicolor/scalable/apps/<APP_ID>.svg
```

The filename **must** equal the application ID. That single name is what
resolves in all three places the icon is used:

- `Icon=io.github.abdullahfanar.FlatBak` in the desktop entry (shell, launcher)
- `application_icon(...)` in the About dialog — `src/ui/dialogs.rs`
- The large icon on the home page — `src/ui/home.rs`

After installing, refresh the caches or the icon will not appear:

```sh
gtk4-update-icon-cache /usr/local/share/icons/hicolor
update-desktop-database /usr/local/share/applications
update-mime-database /usr/local/share/mime
```

`make install` prints these three commands as a reminder.

### The uninstalled fallback

When you run `cargo run` from a source tree, the icon is not in any theme, so
`widgets::own_icon_name()` (in `src/ui/widgets.rs`) substitutes
`drive-harddisk-symbolic` and dims it. That is why the app looks slightly
different from source than when installed — it is not a bug.

### Replacing the artwork

- Keep the 128×128 `viewBox` and roughly 8–10 px of padding inside it, per the
  [GNOME icon HIG](https://developer.gnome.org/hig/guidelines/app-icons.html).
- Keep the filename identical to the application ID.
- Do not reference external images or fonts; keep it a self-contained SVG.
- Flathub additionally wants the icon to render legibly at 64×64. Check that
  before shipping — fine detail disappears at that size.
- If you add a symbolic variant for shell surfaces, name it
  `<APP_ID>-symbolic.svg`, make it a single flat shape, and install it under
  `share/icons/hicolor/symbolic/apps/`.

---

## Changing the wording

FlatBak describes itself in seven places. They are not generated from one
another, so changing the pitch means editing all of them — and a software
centre will happily show a summary that contradicts the README.

| Where | Field | Shown |
| --- | --- | --- |
| `data/<ID>.metainfo.xml` | `<summary>` | Subtitle in software centres |
| `data/<ID>.metainfo.xml` | `<description>` | The full page in software centres |
| `data/<ID>.desktop` | `Comment=` | Tooltip and search result in the launcher |
| `data/<ID>.desktop` | `GenericName=` | Category-style name, e.g. "Flatpak Backup" |
| `src/ui/dialogs.rs` | `.comments(...)` | The About dialog |
| `src/ui/home.rs` | The subtitle label | Under the title on the home page |
| `Cargo.toml` | `description` | Package metadata |

Plus the opening paragraph of `README.md`, which is what people read first on
GitHub.

### Rules that are actually enforced

The AppStream fields are validated, so they carry constraints the others do
not. Checked against `appstreamcli`, with the severity it reports:

| Rule | Tag | Severity |
| --- | --- | --- |
| `<summary>` over ~90 characters | `summary-too-long` | **warning** |
| `<summary>` ending in a full stop | `summary-has-dot-suffix` | info |
| A first `<description>` paragraph that is very short | `description-first-para-too-short` | info |
| Any markup in `<description>` beyond the allowed set | `description-para-markup-invalid` | **error** |

`<description>` accepts `<p>`, `<ul>`, `<ol>` and `<li>`, plus `<em>` and
`<code>` inline. Links are rejected outright — an `<a>` element is a hard
validation error, so put URLs in `<url>` elements instead.

`Comment=` in the desktop entry should say roughly what `<summary>` says.
Keeping the two aligned is a courtesy to anyone reading search results.

After editing either file, re-validate:

```sh
appstreamcli validate data/io.github.abdullahfanar.FlatBak.metainfo.xml
desktop-file-validate data/io.github.abdullahfanar.FlatBak.desktop
```

### Note on translation

None of these strings are translatable yet — there is no gettext setup, and the
UI strings are English literals in the source. If translation is ever added,
the desktop entry and the metainfo grow `Comment[xx]=` and `xml:lang` variants,
and the Rust strings need wrapping in a gettext call. Worth knowing before
scattering user-visible text through new code: keep it easy to find.

---

## Licensing

The project is **MIT**; `LICENSE` holds the text and the copyright line. The
licence is declared in four further places, which must not drift apart:

| File | Field |
| --- | --- |
| `LICENSE` | The text itself |
| `Cargo.toml` | `license = "MIT"` |
| `data/<ID>.metainfo.xml` | `<project_license>MIT</project_license>` |
| `src/ui/dialogs.rs` | `.license_type(gtk::License::MitX11)` |

`gtk::License::MitX11` is GTK's name for the MIT/X11 licence — there is no
plain `Mit` variant.

The MIT terms cover this project's own code. Its dependencies carry their own
licences — the Rust crates are all permissive (MIT / Apache-2.0), while the
GUI libraries are LGPL-2.1+ and are linked dynamically, which MIT code may do
freely. Static linking would need the LGPL terms read carefully first.

---

## Making a release

1. **Bump the version in `Cargo.toml`.** `src/config.rs` reads it via
   `env!("CARGO_PKG_VERSION")`, so nothing else in the code needs touching.
   It shows up in the About dialog and is written into every manifest as
   `created_by`.

2. **Add a `<release>` entry to the metainfo**, newest first:

   ```xml
   <release version="0.2.0" date="2026-10-01">
     <description>
       <p>What changed, in prose a user would understand.</p>
     </description>
   </release>
   ```

   Software centres show this. A release with no entry looks abandoned.

3. **Run the full check:**

   ```sh
   make clippy && make test && cargo run --release --example selftest
   ```

4. **Validate the metadata:**

   ```sh
   appstreamcli validate data/io.github.abdullahfanar.FlatBak.metainfo.xml
   desktop-file-validate data/io.github.abdullahfanar.FlatBak.desktop
   ```

   Until the repository is public, `appstreamcli` reports `url-not-reachable`
   for the three GitHub URLs. That clears itself once the repo exists.

5. **Commit `Cargo.lock`.** It is committed deliberately: this is a binary, and
   reproducible builds matter more than dependency freshness.

6. **Tag it:** `git tag -a v0.2.0 -m "FlatBak 0.2.0" && git push --tags`.

### Before the first public release

The metainfo has no `<screenshots>` yet. Flathub requires at least one, and
software centres look empty without them. Add:

```xml
<screenshots>
  <screenshot type="default">
    <image>https://raw.githubusercontent.com/AbdullahFanar/FlatBak/main/data/screenshots/home.png</image>
    <caption>Choosing what to back up</caption>
  </screenshot>
</screenshots>
```

`tools/render-ui.sh` produces exactly these images. They must be reachable over
HTTPS, so commit them to the repository and link the raw URLs.

---

## The compatibility contract

**This is the part that will bite you if you are careless.** People restore
backups made by older versions — that is the entire point of the program.

Two independent version numbers, both in `src/config.rs` and
`src/backup/container.rs`:

| Constant | Covers | Where |
| --- | --- | --- |
| `CONTAINER_VERSION` | The binary framing: header layout, trailer | `container.rs` |
| `MANIFEST_FORMAT_VERSION` | The TOML manifest's schema | `config.rs` |

They are separate because the manifest is the thing likely to change, and it
can evolve without touching the framing.

Each has a `_MAX` counterpart naming the highest version this build can *read*.
An archive declaring anything higher is refused with "please update FlatBak"
rather than parsed on a hopeful basis.

### The rules

**Adding a field to the manifest — safe, no version bump.**
Give it `#[serde(default)]` in `src/backup/manifest.rs`. Old builds ignore
unknown keys, so they keep reading new archives. This is covered by the
`ignores_unknown_future_keys` test; do not remove it.

**Changing what an existing field means — bump `MANIFEST_FORMAT_VERSION`**
*and* `MANIFEST_FORMAT_VERSION_MAX`. Then either keep reading the old shape or
refuse it explicitly. Never silently reinterpret.

**Changing the header or trailer layout — bump `CONTAINER_VERSION`** and
`CONTAINER_VERSION_MAX`. The header is a fixed 32 bytes with two reserved
fields (offsets 10–12 and 28–32) precisely so small additions do not need a
new layout.

**Removing a field — do not.** Stop writing it and leave the reader tolerant.

Whatever you change, add a test in `tests/archive_safety.rs` that builds an
archive of the old shape by hand and proves the new reader still handles it.
That file already assembles containers byte by byte, so there is a pattern to
copy.

---

## Things that break quietly

These are real failures already hit during development. None of them produce a
compiler error.

### Icon names disappear from Adwaita

`emblem-ok-symbolic` and `package-x-generic-symbolic` were both removed from
recent `adwaita-icon-theme` releases. Referencing a missing name renders the
"broken image" placeholder with no warning.

Every icon name currently used:

```
application-x-executable-symbolic   dialog-error-symbolic
dialog-information-symbolic         dialog-warning-symbolic
document-save-symbolic              drive-harddisk-symbolic
folder-symbolic                     go-next-symbolic
list-remove-symbolic                network-server-symbolic
object-select-symbolic              open-menu-symbolic
```

Before adding a new one, confirm it exists:

```sh
ls /usr/share/icons/Adwaita/symbolic/*/<name>.svg
```

And check the result visually with `tools/render-ui.sh` — the file existing on
disk is necessary but not sufficient, because a stale `icon-theme.cache` can
still hide it.

### The `flatpak` CLI output format

`src/flatpak/cli.rs` parses tab-separated `flatpak list --columns=...` output.
The column set is requested explicitly and each column carries a `:f` suffix to
disable ellipsization. A row with fewer fields than expected is skipped rather
than mis-parsed.

If Flatpak ever renames or reorders a column this breaks silently — the app
would just show no applications. `cargo run --example probe` is the fastest way
to check; it prints exactly what the parser produced.

The commands relied on: `--version`, `list --app`, `remotes`, `remote-add
--if-not-exists`, `install --noninteractive --or-update`.

### Sandbox and container detection

`Flatpak::detect()` picks how to reach the host's Flatpak:

| Situation | Command used |
| --- | --- |
| Normal desktop | `flatpak` |
| Inside a Flatpak (`/.flatpak-info` exists) | `flatpak-spawn --host flatpak` |
| Inside a container | `distrobox-host-exec flatpak` |

Container detection looks for `/run/.containerenv`, `/run/.toolboxenv`,
`/.dockerenv`, or the `CONTAINER_ID` / `DISTROBOX_ENTER_PATH` / `container`
environment variables. Docker-backed distrobox drops **only** `/.dockerenv`,
which is why the list is that long — a narrower check silently fell through to
the container's own empty Flatpak installation.

`FLATBAK_FLATPAK_CMD` overrides all of it.

### Symlinks in application data

Application data contains absolute symlinks pointing back into itself — libvirt
writes its autostart links that way. Storing them verbatim would break on a
machine with a different username, so `plan_symlink()` in `src/backup/writer.rs`
rewrites them as relative. Links pointing anywhere else are dropped and
reported during the backup.

The corresponding check on the reading side, `symlink_target_is_contained()` in
`src/util.rs`, takes the link's **depth** below the application root. Getting
that wrong in either direction is a security hole or silent data loss, so it has
dedicated unit tests. Do not "simplify" it into a plain `..`-rejection.

---

## Development workflow

### Building

Needs Rust 1.80+, GTK 4.12+ and Libadwaita 1.6+ development packages. See the
README for per-distribution package names.

```sh
make            # debug build
make check      # cargo check, fast
make test       # 37 tests: unit + integration
make clippy     # lints, warnings denied — keep this clean
make release
```

### The three development helpers

```sh
# What FlatBak sees of this machine's Flatpak state.
cargo run --example probe

# A real backup and restore cycle against your own application data, restored
# into a scratch directory and compared byte for byte. Nothing is modified.
cargo run --release --example selftest
cargo run --release --example selftest -- org.telegram.desktop

# Render every page to PNG without a desktop session. Needs `weston`.
cargo build --examples && tools/render-ui.sh /tmp/shots
```

`selftest` is the one that matters. It has already caught bugs the unit tests
could not, because real application data contains sockets, absolute symlinks
and files that change mid-read. **Run it before every release.**

### Testing conventions

- `tests/roundtrip.rs` and `tests/archive_safety.rs` set `FLATBAK_DATA_ROOT` to
  a scratch directory. That variable is process-wide, and cargo runs each test
  *file* in its own process but its tests in threads — so anything depending on
  a specific data root belongs in one test function, or its own file.
- `FLATBAK_DATA_ROOT` also lets you point a running FlatBak at a fake
  `~/.var/app` for manual testing. Use it; do not experiment against real data.
- Restores in tests set `install: false`, so no `flatpak` process is spawned and
  the tests work offline.

### Suggested CI

There is no CI yet. A GitHub Actions job on `ubuntu-latest` installing
`libgtk-4-dev libadwaita-1-dev` and running `make clippy && make test` would
cover everything except the UI rendering.

---

## Distribution

### Native install

```sh
sudo make install                 # PREFIX=/usr/local by default
make install DESTDIR=/tmp/pkg PREFIX=/usr    # for packaging
sudo make uninstall
```

Installs five files: the binary, the desktop entry, the icon, the AppStream
metadata and the MIME definition.

### As a Flatpak

FlatBak is a Flatpak tool, so shipping it as a Flatpak is natural. The
manifest is `io.github.abdullahfanar.FlatBak.json` at the repository root; it
builds against `org.gnome.Platform`//50 with the `rust-stable` SDK extension,
and reuses `make install PREFIX=/app` as its only build command, so there is
one place — the Makefile — that knows how to install FlatBak.

Build and run it locally with:

```sh
flatpak-builder --user --install --force-clean build-dir \
  io.github.abdullahfanar.FlatBak.json
flatpak run io.github.abdullahfanar.FlatBak
```

The permissions in `finish-args` are the minimum this app actually needs, and
two of them are easy to get wrong:

```jsonc
"finish-args": [
    "--share=ipc",
    "--socket=fallback-x11",
    "--socket=wayland",
    "--device=dri",

    // Reach the host's flatpak. Without this, flatpak-spawn --host fails and
    // the app can see no applications at all.
    "--talk-name=org.freedesktop.Flatpak",
    // Application data. NOTE: --filesystem=home does NOT cover ~/.var/app —
    // Flatpak deliberately excludes it. You must ask for it by name.
    "--filesystem=~/.var/app/",
    // Read-only: exported icons of other apps, for the installed-apps list.
    // register_icon_paths() in src/ui/mod.rs looks for both of these.
    "--filesystem=/var/lib/flatpak/:ro",
    "--filesystem=~/.local/share/flatpak/:ro"
]
```

`Flatpak::detect()` already handles the sandbox case: it finds `/.flatpak-info`
and switches to `flatpak-spawn --host flatpak`, which then runs on the host
directly — that is also why FlatBak itself needs no `--share=network` or
`--filesystem=xdg-download`. It never touches the network or an arbitrary save
path itself: `flatpak install`/`uninstall` run on the host via the talk-name
above, and choosing where to read or write a `.flatbak` file goes through
GTK's portal-backed `FileDialog` (`src/ui/dialogs.rs`), which grants access to
just the chosen file without any static filesystem permission.

Flathub builds offline, so the dependencies are vendored ahead of time:
`generated-sources.json`, also at the repository root, lists every crate as a
`static.crates.io` download with its hash, generated from `Cargo.lock` by
[`flatpak-cargo-generator.py`](https://github.com/flatpak/flatpak-builder-tools/tree/master/cargo).
**Regenerate it whenever `Cargo.lock` changes:**

```sh
python3 flatpak-cargo-generator.py Cargo.lock -o generated-sources.json
```

The manifest's `flatbak` module sets `CARGO_HOME` to
`/run/build/flatbak/cargo` (the module name matters here — it must match
`build-options.env.CARGO_HOME`) so Cargo picks up the vendored source
replacement that `generated-sources.json` installs there, and
`CARGO_NET_OFFLINE=true` so a build that isn't fully vendored fails loudly
instead of hanging.

---

## What FlatBak writes on a user's machine

Worth knowing when debugging a report, and the complete list:

| Path | What |
| --- | --- |
| `$XDG_DATA_HOME/flatbak/recent.toml` | The recent backups list |
| `~/.var/app/<APP_ID>/` | Restored application data (written, never read back except to detect conflicts) |
| Wherever the user chose | The `.flatbak` archive |

That is all of it. FlatBak has no GSettings schema, no cache and no config file.
A corrupt `recent.toml` is ignored rather than reported — the worst outcome is
an empty list on the home page.

Two environment variables affect behaviour:

| Variable | Effect |
| --- | --- |
| `FLATBAK_FLATPAK_CMD` | Overrides how `flatpak` is invoked, whitespace-split |
| `FLATBAK_DATA_ROOT` | Overrides the location of `~/.var/app` |
