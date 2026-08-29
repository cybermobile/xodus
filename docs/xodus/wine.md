# Wine integration

Running games requires the **xodus wine fork**:
[xodus-gaming/wine](https://github.com/xodus-gaming/wine), branch
`bleeding-edge` — a fork of Valve's
[ValveSoftware/wine](https://github.com/ValveSoftware/wine) (Proton's
bleeding-edge wine, currently based on Wine 11.0) with the xodus patches on
top. Stock wine cannot run Xodus installs: game executables are stored on
disk in encrypted form (`KEEP_ENCRYPTED_ON_DISK`, mirroring how Windows
stores them), they are never written to disk decrypted, and the decrypted
images exist only as anonymous in-memory files for the lifetime of a launch.

Last verified against commit `b1dd32734a34472a28eb5be9922df06e07ac0834`
(with the `dlls/xgameruntime` submodule at `64aebcabb8c66121eae25d3bf0ace4b582ebb0da`).

What the fork adds over Valve wine:

- `WINE_DLL_FILE_MAP` support in `dlls/ntdll/unix/loader.c`
  (`open_mapped_dll_file`) — serve an inherited fd instead of the on-disk
  file when a mapped NT path is loaded.
- `ntdll` support for loading images from memfds.
- `dlls/xgameruntime` — an open-source `xgameruntime.dll` implementation
  ([xodus-gaming/xgameruntime](https://github.com/xodus-gaming/xgameruntime),
  vendored as a git submodule and built as a regular wine module), so GDK
  titles find the runtime DLL that Gaming Services would provide on Windows.
- Game-specific patchsets (e.g. Minecraft GDK) and
  `tools/wine-dll-file-map-launcher.c`, a standalone helper that builds a
  one-entry map from a file and execs wine (useful for testing the
  mechanism without xodus-cli).

## Building the fork

```bash
git clone --recurse-submodules -b bleeding-edge https://github.com/xodus-gaming/wine
cd wine
mkdir build && cd build
../configure --enable-win64   # see wine's README.md for the full dependency list
make -j$(nproc)
```

`--recurse-submodules` matters: `dlls/xgameruntime` is a submodule and is
wired into wine's configure (`WINE_CONFIG_MAKEFILE(dlls/xgameruntime)`), so
a checkout without it will not build the runtime DLL. Then point xodus at
the result:

```bash
XODUS_WINE=/path/to/wine/build/wine xodus-cli run <game-dir>
```

## How `xodus-cli run` launches a game

For every `keep_encrypted` file in the package, `run`:

1. creates an anonymous memory file (`memfd_create` on Linux, a file on a
   transient ramdisk on macOS),
2. decrypts the executable's pages into it (`XvdFile::mount_mem_fd`),
3. clears `FD_CLOEXEC` so the fd survives `exec` into wine,
4. records a mapping from the inherited fd number to the NT path the game
   will use to open its own binary.

The mappings are passed to wine in the `WINE_DLL_FILE_MAP` environment
variable and wine is spawned with the entry executable's NT path as the
argument. See `crates/xodus-cli/src/commands/run.rs`.

## The `WINE_DLL_FILE_MAP` contract

Format: `|`-separated entries of `<fd>:<nt-path>`, e.g.

```
17:\??\Z:\home\user\games\Game\Content\Game.exe|19:\??\Z:\home\user\games\Game\Content\Tool.exe
```

- `<fd>` is the fd number inherited by the wine process (decimal).
- `<nt-path>` is the NT-namespace path of the image being loaded. The
  match is an **exact, case-sensitive byte comparison** of the full NT path
  (UTF-8) — no normalization is applied on the wine side, so the paths
  xodus generates must byte-match what wine resolves when loading the
  image. xodus generates them under the `Z:` drive from the absolute host
  path of the install directory, with `/` replaced by `\`.
- On a match, wine turns the fd into a handle (`GENERIC_READ | SYNCHRONIZE`)
  and maps it `SEC_IMAGE`; malformed entries are skipped silently.

## Selecting the wine binary

`xodus-cli run <source> [wine]` resolves wine in this order:

1. the explicit `[wine]` argument (path, or a name looked up in `PATH`),
2. the `XODUS_WINE` environment variable,
3. `xodus-wine` in `PATH`,
4. `wine` in `PATH` — accepted with a warning, since a stock build will fail
   to load the encrypted executables.

If none is found, `run` exits with an error instead of launching.

## Selecting the executable

If the package contains a single `.exe`, it is launched. If it contains
several, `run` lists them and requires `--exe`; matching is case-insensitive,
accepts `/` or `\` separators, and matches a whole path suffix — e.g.
`--exe Game.exe` or `--exe Binaries/Win64/Game.exe`.

## Related xodus-gaming repositories

- [xodus-gaming/xgameruntime](https://github.com/xodus-gaming/xgameruntime) —
  the `xgameruntime.dll` implementation (also usable standalone).
- [xodus-gaming/xgameruntime-docs](https://github.com/xodus-gaming/xgameruntime-docs) —
  notes on `xgameruntime.dll` internals.
- [xodus-gaming/Proton](https://github.com/xodus-gaming/Proton) — fork of
  [ValveSoftware/Proton](https://github.com/ValveSoftware/Proton) for Steam
  Play integration.
- [xodus-gaming/xal-rs](https://github.com/xodus-gaming/xal-rs) — fork of
  [OpenXbox/xal-rs](https://github.com/OpenXbox/xal-rs), the Xbox
  Authentication Library this repo pins as the `xal` git dependency.
- [xodus-gaming/ntfs](https://github.com/xodus-gaming/ntfs) — fork of
  [ColinFinck/ntfs](https://github.com/ColinFinck/ntfs) used for MSIXVC
  parsing (already wired into this repo via `[patch.crates-io]`).
