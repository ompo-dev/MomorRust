---
title: CLI Reference
description: "Reference for Momor's command-line interface (CLI), including opening files and directories, integrating with tools, and controlling Momor from scripts."
---

# CLI Reference

Use Momor's command-line interface (CLI) to open files and directories, integrate with other tools, and control Momor from scripts.

## Installation

**macOS:** Run the `cli: install` command from the command palette ({#kb command_palette::Toggle}) to install the `momor` CLI to `/usr/local/bin/momor`.

**Linux:** The CLI is included with Momor packages. The binary name may vary by distribution (commonly `momor` or `zeditor`).

**Windows:** The CLI is included with Momor. Add Momor's installation directory to your PATH, or use the full path to `momor.exe`.

## Usage

```sh
momor [OPTIONS] [PATHS]...
```

## Opening Files and Directories

Open a file:

```sh
momor myfile.txt
```

Open a directory as a workspace:

```sh
momor ~/projects/myproject
```

Open multiple files or directories:

```sh
momor file1.txt file2.txt ~/projects/myproject
```

Open a file at a specific line and column:

```sh
momor myfile.txt:42        # Open at line 42
momor myfile.txt:42:10     # Open at line 42, column 10
```

## Options

### `-w`, `--wait`

Wait for all opened files to be closed before the CLI exits. When opening a directory, waits until the window is closed.

This is useful for integrating Momor with tools that expect an editor to block until editing is complete (e.g., `git commit`):

```sh
export EDITOR="momor --wait"
git commit  # Opens Momor and waits for you to close the commit message file
```

### `-n`, `--new`

Open paths in a new workspace window, even if the paths are already open in an existing window:

```sh
momor -n ~/projects/myproject
```

### `-a`, `--add`

Add paths to the currently focused workspace instead of opening a new window. When multiple workspace windows are open, files open in the focused window:

```sh
momor -a newfile.txt
```

### `-r`, `--reuse`

Reuse an existing window, replacing its current workspace with the new paths:

```sh
momor -r ~/projects/different-project
```

### `--diff <OLD_PATH> <NEW_PATH>`

Open a diff view comparing two files. Can be specified multiple times:

```sh
momor --diff file1.txt file2.txt
momor --diff old.rs new.rs --diff old2.rs new2.rs
```

### `--foreground`

Run Momor in the foreground, keeping the terminal attached. Useful for debugging:

```sh
momor --foreground
```

### `--user-data-dir <DIR>`

Use a custom directory for all user data (database, extensions, logs) instead of the default location:

```sh
momor --user-data-dir ~/.momor-custom
```

Default locations:

- **macOS:** `~/Library/Application Support/Momor`
- **Linux:** `$XDG_DATA_HOME/momor` (typically `~/.local/share/momor`)
- **Windows:** `%LOCALAPPDATA%\Momor`

### `-v`, `--version`

Print Momor's version and exit:

```sh
momor --version
```

### `--uninstall`

Uninstall Momor and remove all related files (macOS and Linux only):

```sh
momor --uninstall
```

### `--momor <PATH>`

Specify a custom path to the Momor application or binary:

```sh
momor --momor /path/to/Momor.app myfile.txt
```

## Reading from Standard Input

Read content from stdin by passing `-` as the path:

```sh
echo "Hello, World!" | momor -
cat myfile.txt | momor -
ps aux | momor -
```

This creates a temporary file with the stdin content and opens it in Momor.

## URL Handling

The CLI can open `momor://`, `http://`, and `https://` URLs:

```sh
momor momor://settings
momor https://github.com/momor-industries/momor
```

## Using Momor as Your Default Editor

Set Momor as your default editor for Git and other tools:

```sh
export EDITOR="momor --wait"
export VISUAL="momor --wait"
```

Add these lines to your shell configuration file (e.g., `~/.bashrc`, `~/.zshrc`).

## macOS: Switching Release Channels

On macOS, you can launch a specific release channel by passing the channel name as the first argument:

```sh
momor --stable myfile.txt
momor --preview myfile.txt
momor --nightly myfile.txt
```

## WSL Integration (Windows)

On Windows, the CLI supports opening paths from WSL distributions. This is handled automatically when launching Momor from within WSL.

## Exit Codes

| Code | Meaning                           |
| ---- | --------------------------------- |
| `0`  | Success                           |
| `1`  | Error (details printed to stderr) |

When using `--wait`, the exit code reflects whether the files were saved before closing.
