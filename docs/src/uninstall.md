---
title: Uninstall
description: "This guide covers how to uninstall Momor on different operating systems."
---

# Uninstall

This guide covers how to uninstall Momor on different operating systems.

## macOS

### Standard Installation

If you installed Momor by downloading it from the website:

1. Quit Momor if it's running
2. Open Finder and go to your Applications folder
3. Drag Momor to the Trash (or right-click and select "Move to Trash")
4. Empty the Trash

### Homebrew Installation

If you installed Momor using Homebrew, use the following command:

```sh
brew uninstall --cask momor
```

Or for the preview version:

```sh
brew uninstall --cask momor@preview
```

### Removing User Data (Optional)

To completely remove all Momor configuration files and data:

1. Open Finder
2. Press `Cmd + Shift + G` to open "Go to Folder"
3. Delete the following directories if they exist:
   - `~/Library/Application Support/Momor`
   - `~/Library/Saved Application State/dev.momor.Momor.savedState`
   - `~/Library/Logs/Momor`
   - `~/Library/Caches/dev.momor.Momor`
   - `~/Library/Caches/Momor`
   - `~/.config/momor`
   - `~/.local/state/Momor`

## Linux

### Standard Uninstall

If Momor was installed using the default installation script, run:

```sh
momor --uninstall
```

You'll be prompted whether to keep or delete your preferences. After making a choice, you should see a message that Momor was successfully uninstalled.

If the `momor` command is not found in your PATH, try:

```sh
$HOME/.local/bin/momor --uninstall
```

or:

```sh
$HOME/.local/momor.app/bin/momor --uninstall
```

### Package Manager

If you installed Momor using a package manager (such as Flatpak, Snap, or a distribution-specific package manager), consult that package manager's documentation for uninstallation instructions.

### Manual Removal

If the uninstall command fails or Momor was installed to a custom location, you can manually remove:

- Installation directory: `~/.local/momor.app` (or your custom installation path)
- Binary symlink: `~/.local/bin/momor`
- Configuration and data: `~/.config/momor`

## Windows

### Standard Installation

1. Quit Momor if it's running
2. Open Settings (Windows key + I)
3. Go to "Apps" > "Installed apps" (or "Apps & features" on Windows 10)
4. Search for "Momor"
5. Click the three dots menu next to Momor and select "Uninstall"
6. Follow the prompts to complete the uninstallation

Alternatively, you can:

1. Open the Start menu
2. Right-click on Momor
3. Select "Uninstall"

### Removing User Data (Optional)

To completely remove all Momor configuration files and data:

1. Press `Windows key + R` to open Run
2. Type `%APPDATA%` and press Enter
3. Delete the `Momor` folder if it exists
4. Press `Windows key + R` again, type `%LOCALAPPDATA%` and press Enter
5. Delete the `Momor` folder if it exists

## Troubleshooting

If you encounter issues during uninstallation:

- **macOS/Windows**: Ensure Momor is completely quit before attempting to uninstall. Check Activity Manager (macOS) or Task Manager (Windows) for any running Momor processes.
- **Linux**: If the uninstall script fails, check the error message and consider manual removal of the directories listed above.
- **All platforms**: If you want to start fresh while keeping Momor installed, you can delete the configuration directories instead of uninstalling the application entirely.

For additional help, see our [Linux-specific documentation](./linux.md) or visit the [Momor community](https://momor.dev/community-links).
