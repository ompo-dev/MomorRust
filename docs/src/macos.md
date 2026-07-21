---
title: Momor on macOS
description: "Momor is developed primarily on macOS, making it a first-class platform with full feature support."
---

# Momor on macOS

Momor is developed primarily on macOS, making it a first-class platform with full feature support.

## Installing Momor

Download Momor from the [download page](https://momor.dev/download). The download is a `.dmg` file—open it and drag Momor to your Applications folder.

For the preview build, which receives updates about a week ahead of stable, visit the [preview releases page](https://momor.dev/releases/preview).

After installation, Momor checks for updates automatically and prompts you when a new version is available.

### Homebrew

You can also install Momor using Homebrew:

```sh
brew install --cask momor
```

For the preview version:

```sh
brew install --cask momor@preview
```

### Building from Source

To build Momor from source, see the [macOS development documentation](./development/macos.md).

## System Requirements

- macOS 10.15.7 (Catalina) or later
- Apple Silicon (M1/M2/M3/M4) or Intel processor

Momor uses Metal for GPU-accelerated rendering, which is available on all supported macOS versions.

## Installing the CLI

Momor includes a command-line tool for opening files and projects from Terminal. To install it:

1. Open Momor
2. Open the command palette with `Cmd+Shift+P`
3. Run `cli: install`

This creates a `momor` command in `/usr/local/bin`. You can then open files and folders:

```sh
momor .                    # Open current folder
momor file.txt             # Open a file
momor project/ file.txt    # Open a folder and a file
```

See the [CLI Reference](./reference/cli.md) for all available options.

## Uninstall

1. Quit Momor if it's running
2. Drag Momor from Applications to the Trash
3. Optionally, remove your settings and extensions:

```sh
rm -rf ~/.config/momor
rm -rf ~/Library/Application\ Support/Momor
rm -rf ~/Library/Caches/Momor
rm -rf ~/Library/Logs/Momor
rm -rf ~/Library/Saved\ Application\ State/dev.momor.Momor.savedState
```

If you installed the CLI, remove it with:

```sh
rm /usr/local/bin/momor
```

## Troubleshooting

### Momor won't open or shows "damaged" warning

If macOS reports that Momor is damaged or can't be opened, it's likely a Gatekeeper issue. Try:

1. Right-click (or Control-click) on Momor in Applications
2. Select "Open" from the context menu
3. Click "Open" in the dialog that appears

This tells macOS to trust the application.

If that doesn't work, remove the quarantine attribute:

```sh
xattr -cr /Applications/Momor.app
```

### CLI command not found

If the `momor` command isn't available after installation:

1. Check that `/usr/local/bin` is in your PATH
2. Try reinstalling the CLI via `cli: install` in the command palette
3. Open a new terminal window to reload your PATH

### GPU or rendering issues

Momor uses Metal for rendering. If you experience graphical glitches:

1. Ensure macOS is up to date
2. Restart your Mac to reset the GPU state
3. Check Activity Monitor for GPU pressure from other apps

### High memory or CPU usage

If Momor uses more resources than expected:

1. Check for runaway language servers in the terminal output (`momor: open log`)
2. Try disabling extensions one by one to identify conflicts
3. For large projects, consider using [project settings](./reference/all-settings.md#file-scan-exclusions) to exclude unnecessary folders from indexing

For additional help, see the [Troubleshooting guide](./troubleshooting.md) or visit the [Momor Discord](https://discord.gg/momor-community).
