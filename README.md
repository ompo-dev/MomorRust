# Momor

[![Momor](https://img.shields.io/endpoint?url=https://raw.githubusercontent.com/momor-industries/momor/main/assets/badge/v0.json)](https://momor.dev)
[![CI](https://github.com/momor-industries/momor/actions/workflows/run_tests.yml/badge.svg)](https://github.com/momor-industries/momor/actions/workflows/run_tests.yml)

Welcome to Momor, a high-performance, multiplayer code editor from the creators of [Atom](https://github.com/atom/atom) and [Tree-sitter](https://github.com/tree-sitter/tree-sitter).

---

### Installation

On macOS, Linux, and Windows you can [download Momor directly](https://momor.dev/download) or install Momor via your local package manager ([macOS](https://momor.dev/docs/installation#macos)/[Linux](https://momor.dev/docs/linux#installing-via-a-package-manager)/[Windows](https://momor.dev/docs/windows#package-managers)).

Other platforms are not yet available:

- Web ([tracking issue](https://github.com/momor-industries/momor/issues/5396))

### Developing Momor

- [Building Momor for macOS](./docs/src/development/macos.md)
- [Building Momor for Linux](./docs/src/development/linux.md)
- [Building Momor for Windows](./docs/src/development/windows.md)

### Contributing

See [CONTRIBUTING.md](./CONTRIBUTING.md) for ways you can contribute to Momor.

Also... we're hiring! Check out our [jobs](https://momor.dev/jobs) page for open roles.

### Licensing

License information for third party dependencies must be correctly provided for CI to pass.

We use [`cargo-about`](https://github.com/EmbarkStudios/cargo-about) to automatically comply with open source licenses. If CI is failing, check the following:

- Is it showing a `no license specified` error for a crate you've created? If so, add `publish = false` under `[package]` in your crate's Cargo.toml.
- Is the error `failed to satisfy license requirements` for a dependency? If so, first determine what license the project has and whether this system is sufficient to comply with this license's requirements. If you're unsure, ask a lawyer. Once you've verified that this system is acceptable add the license's SPDX identifier to the `accepted` array in `script/licenses/momor-licenses.toml`.
- Is `cargo-about` unable to find the license for a dependency? If so, add a clarification field at the end of `script/licenses/momor-licenses.toml`, as specified in the [cargo-about book](https://embarkstudios.github.io/cargo-about/cli/generate/config.html#crate-configuration).

## Sponsorship

Momor is developed by **Momor Industries, Inc.**, a for-profit company.

If you’d like to financially support the project, you can do so via GitHub Sponsors.
Sponsorships go directly to Momor Industries and are used as general company revenue.
There are no perks or entitlements associated with sponsorship.
