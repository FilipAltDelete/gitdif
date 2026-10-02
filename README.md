# gitdif

A terminal diff viewer with a LazyVim look. For each changed file it shows the
changed lines, the first and last 20 lines of the file, and folds everything
else into `··· N lines hidden ···` rows you can open on demand. Deleted lines
are shown in red above the place they were removed from.

Run it without arguments to get an explorer of every changed (and untracked)
file in the repository, or pass a path to open one file directly.

## Requirements

- [Rust](https://rustup.rs) 1.88 or newer (to build)
- `git` in your `PATH` at runtime
- A [Nerd Font](https://www.nerdfonts.com) in your terminal for the file and fold icons

## Install

```sh
git clone git@github.com:FilipAltDelete/gitdif.git
cd gitdif
./install.sh
```

`install.sh` builds a release binary and installs it to `~/.local/bin/gitdif`.

| Command | What it does |
|---|---|
| `./install.sh` | build and install to `~/.local/bin` |
| `./install.sh --prefix /usr/local` | install to `/usr/local/bin` (uses `sudo` if the directory isn't writable) |
| `PREFIX=/usr/local ./install.sh` | same as `--prefix` |
| `./install.sh --uninstall` | remove the installed binary |
| `./install.sh --help` | show usage |

To update, pull and run `./install.sh` again.

### Old `cargo install` copies

If `gitdif` was ever installed with `cargo install --path .`, that copy lives in
`~/.cargo/bin`, which usually comes before `~/.local/bin` in `PATH` and keeps
running instead of the new build. `install.sh` (and `--uninstall`) runs
`cargo uninstall gitdif` to remove it, and warns if `gitdif` in your `PATH`
still resolves to some other file.

Your shell caches command locations, so after the old copy is removed run:

```sh
hash -r
```

or open a new terminal.

## Usage

```
gitdif [OPTIONS] [PATH]
```

`PATH` is relative to the current directory. Without it, every changed file in
the repository is listed in the explorer.

| Option | Default | |
|---|---|---|
| `-n`, `--lines <N>` | `20` | lines to show at the top and bottom of each file |
| `-C`, `--context <N>` | `0` | context lines around each change |
| `-b`, `--base <REV>` | `HEAD` | revision to diff against |
| `-p`, `--print` | | print to stdout instead of opening the TUI |
| `-h`, `--help` | | show help |

Examples:

```sh
gitdif                          # browse all changes against HEAD
gitdif src/main.rs              # open one file
gitdif -b main                  # everything changed since the main branch
gitdif -C 3 -n 10 src/main.rs   # 3 lines of context, 10 at top/bottom
gitdif -p src/main.rs           # print with colors instead of the TUI
gitdif src/main.rs > diff.txt   # plain text when redirected
```

When stdout isn't a terminal (piped or redirected), gitdif prints instead of
opening the TUI. Colors are off in that case, or when `NO_COLOR` is set.

## Keys

Press `?` inside gitdif for this list, or `<Space>` for the leader menu.

### Moving

| Key | Action |
|---|---|
| `j` / `k` | down / up |
| `Ctrl+J` / `Ctrl+K` | 10 lines down / up |
| `Ctrl+D` / `Ctrl+U` | half page down / up |
| `Ctrl+F` / `Ctrl+B`, `PgDn` / `PgUp` | page down / up |
| `gg` / `G` | top / bottom |
| `]h` / `[h`, `n` / `N` | next / previous change |
| `h` / `l`, `0` | scroll left / right, reset |

### Hidden lines

| Key | Action |
|---|---|
| `Enter`, `l`, `zo` | open the hidden-lines row under the cursor |
| mouse click | open the hidden-lines row clicked |
| `Ctrl+Enter` | open all hidden lines in the current file |
| `zR` / `zM` | open / close all hidden lines in every open file |
| `+` / `-` | more / less context around changes |

`Ctrl+Enter` needs a terminal with the kitty keyboard protocol (foot, kitty,
Alacritty, Ghostty, WezTerm). Elsewhere, including inside tmux, it arrives as a
plain `Enter`, so use `zR` instead.

### Buffers (tabs) and explorer

| Key | Action |
|---|---|
| `H` / `L`, `[b` / `]b` | previous / next buffer |
| `Ctrl+W`, `<Space>x` | close the active buffer |
| right-click a tab | close that buffer |
| `Tab`, `Ctrl+H` / `Ctrl+L` | focus explorer / editor |
| `<Space>e` | toggle the explorer |
| `Enter`, `l`, `o`, click | open the file under the cursor in the explorer |

### Staging (in the explorer)

| Key | Action |
|---|---|
| `a` | stage all changes; on a file (or folder) that is already fully staged, unstage just that |
| `A` | `git add` the file under the cursor, or every file in the folder under it |
| `Ctrl+A` | stage all changes (`git add -A`) |
| `Ctrl+D` | unstage all changes (`git reset`; your files are left alone) |

A `✓` next to the status letter means the file is staged: green when all of
its changes are staged, yellow when it has more changes since you staged it.

### Commit & push

| Key | Action |
|---|---|
| `Ctrl+.`, `<Space>c` | write a commit message for the staged changes |
| `Enter` (in the popup) | `git commit`, then `git push` |
| `Esc` (in the popup) | cancel; the message is kept for next time |
| `Backspace`, `Ctrl+W`, `Ctrl+U` (in the popup) | delete a character / word / everything |

The result shows in the message bar. A branch without an upstream is pushed to
`origin` (or your only remote) with `-u`. Pushing can't ask for a password or
SSH passphrase, so use an SSH agent or a git credential helper; if the push
fails, the commit is still made and you can push from the terminal popup.
`Ctrl+.` needs the kitty keyboard protocol (see `Ctrl+Enter` above).

### Terminal

| Key | Action |
|---|---|
| `Ctrl+:`, `<Space>t` | open a terminal popup in the folder gitdif was started from |
| `Ctrl+:` (in the popup) | hide it; the shell keeps running and comes back on the next open |
| `exit` | close the shell and the popup |
| mouse wheel | scroll back through output; typing jumps back down |

The popup runs your `$SHELL`. While it is open, every key goes to the shell.
`Ctrl+:` needs the kitty keyboard protocol (see `Ctrl+Enter` above); use
`<Space>t` to open it elsewhere. A running shell is closed when gitdif quits.

### Other

| Key | Action |
|---|---|
| `R`, `<Space>r` | refresh git state |
| `?` | key help |
| `q`, `<Space>q`, `Ctrl+C` | quit |
