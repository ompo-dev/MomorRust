---
title: Building Momor for Linux
description: "Guide to building momor for linux for Momor development."
---

# Building Momor for Linux

## Repository

Clone the [Momor repository](https://github.com/momor-industries/momor).

## Dependencies

- Install [rustup](https://www.rust-lang.org/tools/install)

- Install the necessary system libraries:

  ```sh
  script/linux
  ```

  If you prefer to install the system libraries manually, you can find the list of required packages in the `script/linux` file.

## Building from source

Once the dependencies are installed, you can build Momor using [Cargo](https://doc.rust-lang.org/cargo/).

For a debug build of the editor:

```sh
cargo run
```

And to run the tests:

```sh
cargo test --workspace
```

In release mode, the primary user interface is the `cli` crate. You can run it in development with:

```sh
cargo run -p cli
```

## Installing a development build

You can install a local build on your machine with:

```sh
./script/install-linux
```

This builds `momor` and the `cli` in release mode, installs the binary at `~/.local/bin/momor`, and installs `.desktop` files to `~/.local/share`.

> **_Note_**: If you encounter linker errors similar to the following:
>
> ```bash
> error: linking with `cc` failed: exit status: 1 ...
> = note: /usr/bin/ld: /tmp/rustcISMaod/libaws_lc_sys-79f08eb6d32e546e.rlib(f8e4fd781484bd36-bcm.o): in function `aws_lc_0_25_0_handle_cpu_env':
>           /aws-lc/crypto/fipsmodule/cpucap/cpu_intel.c:(.text.aws_lc_0_25_0_handle_cpu_env+0x63): undefined reference to `__isoc23_sscanf'
>           /usr/bin/ld: /tmp/rustcISMaod/libaws_lc_sys-79f08eb6d32e546e.rlib(f8e4fd781484bd36-bcm.o): in function `pkey_rsa_ctrl_str':
>           /aws-lc/crypto/fipsmodule/evp/p_rsa.c:741:(.text.pkey_rsa_ctrl_str+0x20d): undefined reference to `__isoc23_strtol'
>           /usr/bin/ld: /aws-lc/crypto/fipsmodule/evp/p_rsa.c:752:(.text.pkey_rsa_ctrl_str+0x258): undefined reference to `__isoc23_strtol'
>           collect2: error: ld returned 1 exit status
>   = note: some `extern` functions couldn't be found; some native libraries may need to be installed or have their path specified
>   = note: use the `-l` flag to specify native libraries to link
>   = note: use the `cargo:rustc-link-lib` directive to specify the native libraries to link with Cargo (see https://doc.rust-lang.org/cargo/reference/build-scripts.html#rustc-link-lib)
> error: could not compile `remote_server` (bin "remote_server") due to 1 previous error
> ```
>
> **Cause**:
> This is caused by known bugs in aws-lc-rs (no GCC >= 14 support): [FIPS fails to build with GCC >= 14](https://github.com/aws/aws-lc-rs/issues/569)
> & [GCC-14 - build failure for FIPS module](https://github.com/aws/aws-lc/issues/2010)
>
> You can refer to [linux: Linker error for remote_server when using script/install-linux](https://github.com/momor-industries/momor/issues/24880) for more information.
>
> **Workaround**:
> Set the remote server target to `x86_64-unknown-linux-gnu` like so `export REMOTE_SERVER_TARGET=x86_64-unknown-linux-gnu; script/install-linux`

## Wayland & X11

Momor supports both X11 and Wayland. By default, we pick whichever we can find at runtime. If you're on Wayland and want to run in X11 mode, use the environment variable `WAYLAND_DISPLAY=''`.

## Notes for packaging Momor

This section is for distribution maintainers packaging Momor.

### Technical requirements

Momor has two main binaries:

- You will need to build `crates/cli` and make its binary available in `$PATH` with the name `momor`.
- You will need to build `crates/momor` and put it at `$PATH/to/cli/../../libexec/momor-editor`. For example, if you are going to put the cli at `~/.local/bin/momor` put momor at `~/.local/libexec/momor-editor`. As some linux distributions (notably Arch) discourage the use of `libexec`, you can also put this binary at `$PATH/to/cli/../../lib/momor/momor-editor` (e.g. `~/.local/lib/momor/momor-editor`) instead.
- If you are going to provide a `.desktop` file you can find a template in `crates/momor/resources/momor.desktop.in`, and use `envsubst` to populate it with the values required. This file should also be renamed to `$APP_ID.desktop` so that the file [follows the FreeDesktop standards](https://github.com/momor-industries/momor/issues/12707#issuecomment-2168742761). You should also make this desktop file executable (`chmod 755`).
- You will need to ensure that the necessary libraries are installed. You can get the current list by [inspecting the built binary](https://github.com/momor-industries/momor/blob/935cf542aebf55122ce6ed1c91d0fe8711970c82/script/bundle-linux#L65-L67) on your system.
- For an example of a complete build script, see [script/bundle-linux](https://github.com/momor-industries/momor/blob/935cf542aebf55122ce6ed1c91d0fe8711970c82/script/bundle-linux).
- You can disable Momor's auto updates and provide instructions for users who try to update Momor manually by building (or running) Momor with the environment variable `MOMOR_UPDATE_EXPLANATION`. For example: `MOMOR_UPDATE_EXPLANATION="Please use flatpak to update momor."`.
- Make sure to update the contents of the `crates/momor/RELEASE_CHANNEL` file to 'nightly', 'preview', or 'stable', with no newline. This will cause Momor to use the credentials manager to remember a user's login.

### Other things to note

Momor moves quickly, and distribution maintainers often have different constraints and priorities. The points below describe current trade-offs:

- Momor is a fast-moving project. We typically publish 2-3 builds per week to address reported issues and ship larger changes.
- There are a couple of other `momor` binaries that may be present on Linux systems ([1](https://openzfs.github.io/openzfs-docs/man/v2.2/8/momor.8.html), [2](https://momor.brimdata.io/docs/commands/momor)). If you want to rename our CLI binary because of these issues, we suggest `zedit`, `zeditor`, or `momor-cli`.
- Momor automatically installs versions of common developer tools, similar to rustup/rbenv/pyenv. This behavior is discussed [here](https://github.com/momor-industries/momor/issues/12589).
- Users can install extensions locally and from [momor-industries/extensions](https://github.com/momor-industries/extensions). Extensions may install additional tools such as language servers. Planned safety improvements are tracked [here](https://github.com/momor-industries/momor/issues/12358).
- Momor connects to several online services by default (AI, telemetry, collaboration). AI and our telemetry can be disabled by your users with their momor settings or by patching our [default settings file](https://github.com/momor-industries/momor/blob/main/assets/settings/default.json).
- Because of the points above, Momor currently does not work well with sandboxes. See [this discussion](https://github.com/momor-industries/momor/pull/12006#issuecomment-2130421220).

## Flatpak

> Momor's current Flatpak integration exits the sandbox on startup. Workflows that rely on Flatpak's sandboxing may not work as expected.

To build & install the Flatpak package locally follow the steps below:

1. Install Flatpak for your distribution as outlined [here](https://flathub.org/setup).
2. Run the `script/flatpak/deps` script to install the required dependencies.
3. Run `script/flatpak/bundle-flatpak`.
4. Now the package has been installed and has a bundle available at `target/release/{app-id}.flatpak`.

## Memory profiling

[`heaptrack`](https://github.com/KDE/heaptrack) is quite useful for diagnosing memory leaks. To install it:

```sh
$ sudo apt install heaptrack heaptrack-gui
$ cargo install cargo-heaptrack
```

Then, to build and run Momor with the profiler attached:

```sh
$ cargo heaptrack -b momor
```

When this momor instance is exited, terminal output will include a command to run `heaptrack_interpret` to convert the `*.raw.zst` profile to a `*.zst` file which can be passed to `heaptrack_gui` for viewing.

## Perf recording

How to get a flamegraph with resolved symbols from a running Momor instance.
Use this when Momor is using a lot of CPU. It is not useful for hangs.

### During the incident

- Find the PID (process ID) using:
  `ps -eo size,pid,comm | grep momor | sort | head -n 1 | cut -d ' ' -f 2`
  Or find the PID of `momor-editor` with the highest RAM usage in something
  like htop/btop/top.

- Install perf:
  On Ubuntu (derivatives) run `sudo apt install linux-tools`.

- Perf record:
  Run `sudo perf record -p <pid you just found>`, wait a few seconds to gather data, then press Ctrl+C. You should now have a `perf.data` file.

- Make the output file user owned:
  run `sudo chown $USER:$USER perf.data`

- Get build info:
  Run momor again and type `momor: about` in the command pallet to get the exact commit.

The `perf.data` file can be sent to Momor together with the exact commit.

### Later

This can be done by Momor staff.

- Build Momor with symbols:
  Check out the commit found previously and modify `Cargo.toml`.
  Apply the following diff, then make a release build.

```diff
[profile.release]
-debug = "limited"
+debug = "full"
```

- Add the symbols to the perf database:
  `perf buildid-cache -v -a <path to release momor binary>`

- Resolve the symbols from the db:
  `perf inject -i perf.data -o perf_with_symbols.data`

- Install flamegraph:
  `cargo install cargo-flamegraph`

- Render the flamegraph:
  `flamegraph --perfdata perf_with_symbols.data`

## Troubleshooting

### Cargo errors claiming that a dependency is using unstable features

Try `cargo clean` and `cargo build`.
