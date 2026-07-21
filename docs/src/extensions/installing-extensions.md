---
title: Installing Extensions
description: "Browse, install, and manage extensions from the Momor Extension Gallery."
---

# Installing Extensions {#installing-extensions}

Extensions add functionality to Momor, including languages, themes, and AI tools. Browse and install them from the Extension Gallery.

Open the Extension Gallery with {#kb momor::Extensions}, or select "Momor > Extensions" from the menu bar.

## Installation Location

- On macOS, extensions are installed in `~/Library/Application Support/Momor/extensions`.
- On Linux, they are installed in either `$XDG_DATA_HOME/momor/extensions` or `~/.local/share/momor/extensions`.
- On Windows, the directory is `%LOCALAPPDATA%\Momor\extensions`.

This directory contains two subdirectories:

- `installed`, which contains the source code for each extension.
- `work` which contains files created by the extension itself, such as downloaded language servers.

## Auto-installing

To automate extension installation/uninstallation see the docs for [auto_install_extensions](../reference/all-settings.md#auto-install-extensions).
