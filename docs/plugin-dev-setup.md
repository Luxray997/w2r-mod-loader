# Setting up your PC to build and test plugins

This guide takes a Windows PC with nothing installed to the point where you can build a plugin,
install your build into Warcraft II Remastered and read its log. It covers the machine only. How a
plugin is written is in the [plugin guide](https://github.com/hastinbe/w2r-plugins/blob/main/docs/PLUGIN-GUIDE.md)
and the [contributing rules](https://github.com/hastinbe/w2r-plugins/blob/main/CONTRIBUTING.md) of
the `w2r-plugins` repository; the interface plugins talk to is in [abi.md](abi.md).

You need Windows 10 or 11, Warcraft II Remastered, and about 8 GB of free disk (the C++ build tools
are most of it).

## 1. Install the tools

### Git

Install [Git for Windows](https://git-scm.com/download/win). The defaults are fine. It also gives
you Git Bash, which the repository's shell scripts expect.

### Visual Studio Build Tools (the C++ linker)

Rust on Windows links with Microsoft's linker, which comes with the Build Tools. You do not need the
full Visual Studio IDE.

1. Download **Build Tools for Visual Studio** from <https://visualstudio.microsoft.com/downloads/>
   (under "Tools for Visual Studio").
2. In the installer tick the workload **Desktop development with C++** and install it.

If `C:` is short on space, the installer lets you pick another drive for the install location.

### Rust

Install Rust with `rustup` from <https://rustup.rs> (download `rustup-init.exe`, accept the defaults),
then open a **new** terminal so the new `PATH` is picked up, and check:

```
rustc --version
cargo --version
```

The plugin repository needs Rust 1.85 or newer (edition 2024). `rustup update` brings an older
install up to date.

### The 32-bit target

The game is a 32-bit program, so a plugin must be a 32-bit DLL. Your normal Rust host is 64-bit; add
the 32-bit target once:

```
rustup target add i686-pc-windows-msvc
```

Every plugin build below passes `--target i686-pc-windows-msvc`. A 64-bit DLL will not load: the
host reports a load error in its log.

### An editor

Anything works. [VS Code](https://code.visualstudio.com/) with the `rust-analyzer` extension is the
usual choice.

## 2. Get the code

Fork [`w2r-plugins`](https://github.com/hastinbe/w2r-plugins) on GitHub (you will open pull requests
from your fork), then clone your fork. Clone the mod loader next to it:

```
mkdir w2r
cd w2r
git clone https://github.com/<you>/w2r-plugins
git clone https://github.com/hastinbe/w2r-mod-loader
```

Keep the two folders side by side. The mod loader repository gives you the ABI header and docs and
builds the `w2r-loader` command line tool used in section 4. The plugin repository is where your
plugin lives, in `plugins/<id>/`. Start a branch for it: `git switch -c plugin/<id>`.

## 3. Build and test

From the `w2r-plugins` folder:

```
cargo run -p w2r-publish -- check                                    # manifests are valid
cargo test -p w2r-random-map                                         # a plugin's unit tests
cargo build --release --target i686-pc-windows-msvc -p w2r-random-map
```

Replace `w2r-random-map` with your crate. The first build downloads and compiles the dependencies and
takes a few minutes; later builds are quick. The DLL ends up in
`target\i686-pc-windows-msvc\release\`, named after the crate with underscores
(`w2r_random_map.dll`).

`cargo test` runs on your normal 64-bit host, so it works without the game. Put your plugin's
decisions in plain Rust functions and test those; the `extern "C"` callbacks should be thin glue.
[`plugins/w2r-random-map`](https://github.com/hastinbe/w2r-plugins/tree/main/plugins/w2r-random-map)
is a good one to copy: it needs no capabilities.

### Building against your own copy of the mod loader

`w2r-plugins` pins the ABI crates to a fixed commit of the mod loader, so a build does not change
underneath you. If you are changing the ABI itself, or want the newest host features before the pin
moves, create the untracked file `w2r-plugins\.cargo\config.toml`:

```toml
[patch."https://github.com/hastinbe/w2r-mod-loader"]
w2r-abi = { path = "../w2r-mod-loader/crates/w2r-abi" }
w2r-formats = { path = "../w2r-mod-loader/crates/w2r-formats" }
w2r-loader-lib = { path = "../w2r-mod-loader/crates/w2r-loader-lib" }
```

Never commit that file. `CONTRIBUTING.md` explains the `Cargo.lock` side effect and the
`tools/sync-lock.sh` helper.

## 4. Try it in the game

Install the mod itself first: run the **W2R Mod Loader** app from the
[releases page](https://github.com/hastinbe/w2r-mod-loader/releases) and click **Install the mod**.
Start the game once so the `w2r` folder exists.

Install your own build the same way a player installs a published one, with a package and a small
local index. The host only loads a plugin whose DLL hash and capabilities are recorded by the
manager, so copying a DLL into the plugins folder by hand does not work; going through the manager
records them for you.

1. **Close the game.** Plugins cannot be changed while it runs.
2. In `w2r-plugins`, package every plugin you built:

   ```
   cargo run -p w2r-publish -- package --target-dir target/i686-pc-windows-msvc/release --out out/packages
   ```

3. Make an index for those packages. The URL template is never contacted (the files are read from
   the folder); it only has to be a valid `https` address:

   ```
   cargo run -p w2r-publish -- index --packages out/packages --url-template "https://example.invalid/{name}.zip" --out out/packages/index.json
   ```

4. In `w2r-mod-loader`, install from that folder (`--index-dir` is a development switch and is not
   listed in `--help`):

   ```
   cargo run -p w2r-loader-cli -- --index-dir ..\w2r-plugins\out\packages install <your-plugin-id>
   ```

   It prints the plugin's capabilities and asks you to approve them, exactly as the app does. A plugin
   that asks for `hooks.raw` also needs `--allow-hooks-raw` (and `--yes` to skip the questions).

5. Start the game. The plugin is enabled after install. Switch it off and on in the Mod Loader's
   **Installed** tab, or with `w2r-loader disable <id>` and `w2r-loader enable <id>`.

To try a change: bump nothing, just rebuild, repackage, and install again with the same command. The
manager treats it as a reinstall of the same version and replaces the files. (A *published* version
is immutable, but your local installs are your own.)

To go back to the real thing, remove your build and install the published one:

```
cargo run -p w2r-loader-cli -- remove <your-plugin-id>
```

### Reading the log

The host writes everything to `w2r.log` in the `w2r` folder, which is
`%USERPROFILE%\Saved Games\Warcraft2Remastered\w2r\logs\` by default (Windows may have moved Saved
Games; open it from the folder's properties if you are unsure). Open it while the game runs. Your
plugin's `log` calls appear with its id, and a plugin that fails to load, fails a check or faults is
named with the reason. A plugin that crashes inside the game is disabled by the host for the rest of
the session; the log says where.

Use the in-game plugin list (reached from the options screen) for plugins that draw settings with
the `W2rUi` calls. Plugin settings are stored in `config\<id>.json` and its own files in
`plugins\<id>\data\`.

### Multiplayer testing

Most features can be tried on your own in a custom game against the computer, but anything about
the lobby or other players needs a second machine, or a friend with the mod. A plugin that sets
`affects_simulation` or uses `hooks.raw` is part of the session fingerprint, so everyone in the game
must have the same build of it; the lobby warns when they do not.

## 5. Checklist before a pull request

```
cargo run --locked -p w2r-publish -- check
cargo test --locked --workspace
cargo build --locked --release --target i686-pc-windows-msvc -p <your-crate>
```

Then open the PR from your branch against `main` and fill in the template; say plainly whether you
ran it in the game. The rules (capabilities, versions, AI-assisted work) are in
[CONTRIBUTING.md](https://github.com/hastinbe/w2r-plugins/blob/main/CONTRIBUTING.md).

## Troubleshooting

| Symptom | Likely cause and fix |
|---|---|
| `linker 'link.exe' not found` | The C++ build tools are not installed, or the terminal was opened before they were. Install the **Desktop development with C++** workload and open a new terminal. |
| `can't find crate for 'core'` / `target may not be installed` | Run `rustup target add i686-pc-windows-msvc`. |
| `rustup could not choose a version of cargo` | `rustup` is installed but has no toolchain: run `rustup default stable`. If you installed Rust to a non-default drive, make sure the `RUSTUP_HOME` and `CARGO_HOME` environment variables are set for every terminal you use, not only one. |
| `cargo` or `rustc` not found right after installing | Open a new terminal; `PATH` is only read at start. |
| `edition 2024 is unstable` / `requires rustc 1.85` | `rustup update`. |
| Install says `approval_needed` | Pass `--allow-hooks-raw` for a plugin that asks for `hooks.raw`. |
| Install says `bad_url` or the index will not parse | The URL template must start with `https://`, contain `{name}` and have no spaces. |
| Plugin does not appear in the game | It is disabled (check **Installed** or `w2r-loader list`), the game build is not in its manifest `game_builds`, or the log names the failure. |
| `E_HASH` in the log | The DLL on disk is not the one the manager recorded. Install again through the manager rather than copying files. |
| Plugin loads but a 64-bit DLL error appears | You built without `--target i686-pc-windows-msvc`. |
| The game or Battle.net refuses to start | Remove the mod (`Uninstall` in the app, or delete `version.dll` from the game's `x86` folder, restoring `version.dll.bak` if one is there) and check the log. |
| Antivirus quarantines the DLL | Unsigned native DLLs sometimes trip heuristics. Add an exclusion for your `target` folder and the `w2r` folder. |

## Keeping up to date

```
rustup update
git pull
```

in both repositories now and then. When the mod loader's ABI changes, the `rev` in the
`w2r-plugins` root `Cargo.toml` moves in its own pull request; pull it before you start new work.
