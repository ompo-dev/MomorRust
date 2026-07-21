#!/usr/bin/env sh
set -eu

# Downloads a tarball from https://momor.dev/releases and unpacks it
# into ~/.local/. If you'd prefer to do this manually, instructions are at
# https://momor.dev/docs/linux.

main() {
    platform="$(uname -s)"
    arch="$(uname -m)"
    channel="${MOMOR_CHANNEL:-stable}"
    MOMOR_VERSION="${MOMOR_VERSION:-latest}"
    # Use TMPDIR if available (for environments with non-standard temp directories)
    if [ -n "${TMPDIR:-}" ] && [ -d "${TMPDIR}" ]; then
        temp="$(mktemp -d "$TMPDIR/momor-XXXXXX")"
    else
        temp="$(mktemp -d "/tmp/momor-XXXXXX")"
    fi

    if [ "$platform" = "Darwin" ]; then
        platform="macos"
    elif [ "$platform" = "Linux" ]; then
        platform="linux"
    else
        echo "Unsupported platform $platform"
        exit 1
    fi

    case "$platform-$arch" in
        macos-arm64* | linux-arm64* | linux-armhf | linux-aarch64)
            arch="aarch64"
            ;;
        macos-x86* | linux-x86* | linux-i686*)
            arch="x86_64"
            ;;
        *)
            echo "Unsupported platform or architecture"
            exit 1
            ;;
    esac

    if command -v curl >/dev/null 2>&1; then
        curl () {
            command curl -fL "$@"
        }
    elif command -v wget >/dev/null 2>&1; then
        curl () {
            wget -O- "$@"
        }
    else
        echo "Could not find 'curl' or 'wget' in your path"
        exit 1
    fi

    "$platform" "$@"

    if [ "$(command -v momor)" = "$HOME/.local/bin/momor" ]; then
        echo "Momor has been installed. Run with 'momor'"
    else
        echo "To run Momor from your terminal, you must add ~/.local/bin to your PATH"
        echo "Run:"

        case "$SHELL" in
            *zsh)
                echo "   echo 'export PATH=\$HOME/.local/bin:\$PATH' >> ~/.zshrc"
                echo "   source ~/.zshrc"
                ;;
            *fish)
                echo "   fish_add_path -U $HOME/.local/bin"
                ;;
            *)
                echo "   echo 'export PATH=\$HOME/.local/bin:\$PATH' >> ~/.bashrc"
                echo "   source ~/.bashrc"
                ;;
        esac

        echo "To run Momor now, '~/.local/bin/momor'"
    fi
}

linux() {
    if [ -n "${MOMOR_BUNDLE_PATH:-}" ]; then
        cp "$MOMOR_BUNDLE_PATH" "$temp/momor-linux-$arch.tar.gz"
    else
        echo "Downloading Momor version: $MOMOR_VERSION"
        curl "https://cloud.momor.dev/releases/$channel/$MOMOR_VERSION/download?asset=momor&arch=$arch&os=linux&source=install.sh" > "$temp/momor-linux-$arch.tar.gz"
    fi

    suffix=""
    if [ "$channel" != "stable" ]; then
        suffix="-$channel"
    fi

    appid=""
    case "$channel" in
      stable)
        appid="dev.momor.Momor"
        ;;
      nightly)
        appid="dev.momor.Momor-Nightly"
        ;;
      preview)
        appid="dev.momor.Momor-Preview"
        ;;
      dev)
        appid="dev.momor.Momor-Dev"
        ;;
      *)
        echo "Unknown release channel: ${channel}. Using stable app ID."
        appid="dev.momor.Momor"
        ;;
    esac

    # Unpack
    rm -rf "$HOME/.local/momor$suffix.app"
    mkdir -p "$HOME/.local/momor$suffix.app"
    tar -xzf "$temp/momor-linux-$arch.tar.gz" -C "$HOME/.local/"

    # Setup ~/.local directories
    mkdir -p "$HOME/.local/bin" "$HOME/.local/share/applications"

    # Link the binary
    if [ -f "$HOME/.local/momor$suffix.app/bin/momor" ]; then
        ln -sf "$HOME/.local/momor$suffix.app/bin/momor" "$HOME/.local/bin/momor"
    else
        # support for versions before 0.139.x.
        ln -sf "$HOME/.local/momor$suffix.app/bin/cli" "$HOME/.local/bin/momor"
    fi

    # Copy .desktop file
    desktop_file_path="$HOME/.local/share/applications/${appid}.desktop"
    src_dir="$HOME/.local/momor$suffix.app/share/applications"
    if [ -f "$src_dir/${appid}.desktop" ]; then
        cp "$src_dir/${appid}.desktop" "${desktop_file_path}"
    else
        # Fallback for older tarballs
        cp "$src_dir/momor$suffix.desktop" "${desktop_file_path}"
    fi
    sed -i "s|Icon=momor|Icon=$HOME/.local/momor$suffix.app/share/icons/hicolor/512x512/apps/momor.png|g" "${desktop_file_path}"
    sed -i "s|Exec=momor|Exec=$HOME/.local/momor$suffix.app/bin/momor|g" "${desktop_file_path}"
}

macos() {
    echo "Downloading Momor version: $MOMOR_VERSION"
    curl "https://cloud.momor.dev/releases/$channel/$MOMOR_VERSION/download?asset=momor&os=macos&arch=$arch&source=install.sh" > "$temp/Momor-$arch.dmg"
    hdiutil attach -quiet "$temp/Momor-$arch.dmg" -mountpoint "$temp/mount"
    app="$(cd "$temp/mount/"; echo *.app)"
    echo "Installing $app"
    if [ -d "/Applications/$app" ]; then
        echo "Removing existing $app"
        rm -rf "/Applications/$app"
    fi
    ditto "$temp/mount/$app" "/Applications/$app"
    hdiutil detach -quiet "$temp/mount"

    mkdir -p "$HOME/.local/bin"
    # Link the binary
    ln -sf "/Applications/$app/Contents/MacOS/cli" "$HOME/.local/bin/momor"
}

main "$@"
