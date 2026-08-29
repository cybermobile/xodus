# Wine integration

Running games requires a **patched wine build** ("xodus wine"). Stock wine
cannot run Xodus installs: game executables are stored on disk in encrypted
form (`KEEP_ENCRYPTED_ON_DISK`, mirroring how Windows stores them), they are
never written to disk decrypted, and the decrypted images exist only as
anonymous in-memory files for the lifetime of a launch.

> **Status:** the patched wine build is not yet shipped or pinned in this
> repository. Until it is, you must build it yourself from the patch series /
> fork used by the project. This is the top item on the roadmap to make
> `xodus-cli run` usable out of the box.

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

The patched wine must honor this variable when resolving NT paths to file
objects: if an opened path matches a mapped NT path, wine must serve the
already-open inherited file descriptor (the decrypted image) instead of the
on-disk file (the encrypted one).

Format: `|`-separated entries of `<fd>:<nt-path>`, e.g.

```
17:\??\Z:\home\user\games\Game\Content\Game.exe|19:\??\Z:\home\user\games\Game\Content\Tool.exe
```

- `<fd>` is the fd number inherited by the wine process (decimal).
- `<nt-path>` is the NT-namespace path wine will see the game open. Paths are
  currently always generated under the `Z:` drive from the absolute host path
  of the install directory, with `/` replaced by `\`.

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
