#!/usr/bin/env sh
set -eu

# Uninstalls Momor that was installed using the install.sh script

check_remaining_installations() {
    platform="$(uname -s)"
    if [ "$platform" = "Darwin" ]; then
        # Check for any Momor variants in /Applications
        remaining=$(ls -d /Applications/Momor*.app 2>/dev/null | wc -l)
        [ "$remaining" -eq 0 ]
    else
        # Check for any Momor variants in ~/.local
        remaining=$(ls -d "$HOME/.local/momor"*.app 2>/dev/null | wc -l)
        [ "$remaining" -eq 0 ]
    fi
}

prompt_remove_preferences() {
    printf "Do you want to keep your Momor preferences? [Y/n] "
    read -r response
    case "$response" in
        [nN]|[nN][oO])
            rm -rf "$HOME/.config/momor"
            echo "Preferences removed."
            ;;
        *)
            echo "Preferences kept."
            ;;
    esac
}

main() {
    platform="$(uname -s)"
    channel="${MOMOR_CHANNEL:-stable}"

    if [ "$platform" = "Darwin" ]; then
        platform="macos"
    elif [ "$platform" = "Linux" ]; then
        platform="linux"
    else
        echo "Unsupported platform $platform"
        exit 1
    fi

    "$platform"

    echo "Momor has been uninstalled"
}

linux() {
    suffix=""
    if [ "$channel" != "stable" ]; then
        suffix="-$channel"
    fi

    appid=""
    db_suffix="stable"
    case "$channel" in
      stable)
        appid="dev.momor.Momor"
        db_suffix="stable"
        ;;
      nightly)
        appid="dev.momor.Momor-Nightly"
        db_suffix="nightly"
        ;;
      preview)
        appid="dev.momor.Momor-Preview"
        db_suffix="preview"
        ;;
      dev)
        appid="dev.momor.Momor-Dev"
        db_suffix="dev"
        ;;
      *)
        echo "Unknown release channel: ${channel}. Using stable app ID."
        appid="dev.momor.Momor"
        db_suffix="stable"
        ;;
    esac

    # Remove the app directory
    rm -rf "$HOME/.local/momor$suffix.app"

    # Remove the binary symlink
    rm -f "$HOME/.local/bin/momor"

    # Remove the .desktop file
    rm -f "$HOME/.local/share/applications/${appid}.desktop"

    # Remove the database directory for this channel
    rm -rf "$HOME/.local/share/momor/db/0-$db_suffix"

    # Remove socket file
    rm -f "$HOME/.local/share/momor/momor-$db_suffix.sock"

    # Remove the entire Momor directory if no installations remain
    if check_remaining_installations; then
        rm -rf "$HOME/.local/share/momor"
        prompt_remove_preferences
    fi

    rm -rf $HOME/.momor_server
}

macos() {
    app="Momor.app"
    db_suffix="stable"
    app_id="dev.momor.Momor"
    case "$channel" in
      nightly)
        app="Momor Nightly.app"
        db_suffix="nightly"
        app_id="dev.momor.Momor-Nightly"
        ;;
      preview)
        app="Momor Preview.app"
        db_suffix="preview"
        app_id="dev.momor.Momor-Preview"
        ;;
      dev)
        app="Momor.app"
        db_suffix="dev"
        app_id="dev.momor.Momor-Dev"
        ;;
    esac

    # Remove the app bundle
    if [ -d "/Applications/$app" ]; then
        rm -rf "/Applications/$app"
    fi

    # Remove the binary symlink
    rm -f "$HOME/.local/bin/momor"

    # Remove the database directory for this channel
    rm -rf "$HOME/Library/Application Support/Momor/db/0-$db_suffix"

    # Remove app-specific files and directories
    rm -rf "$HOME/Library/Application Support/com.apple.sharedfilelist/com.apple.LSSharedFileList.ApplicationRecentDocuments/$app_id.sfl"*
    rm -rf "$HOME/Library/Caches/$app_id"
    rm -rf "$HOME/Library/HTTPStorages/$app_id"
    rm -rf "$HOME/Library/Preferences/$app_id.plist"
    rm -rf "$HOME/Library/Saved Application State/$app_id.savedState"

    # Remove the entire Momor directory if no installations remain
    if check_remaining_installations; then
        rm -rf "$HOME/Library/Application Support/Momor"
        rm -rf "$HOME/Library/Logs/Momor"

        prompt_remove_preferences
    fi

    rm -rf $HOME/.momor_server
}

main "$@"
