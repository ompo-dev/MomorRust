# Momor Extensions

This directory contains extensions for Momor that are largely maintained by the Momor team. They currently live in the Momor repository for ease of maintenance.

If you are looking for the Momor extension registry, see the [`momor-industries/extensions`](https://github.com/momor-industries/extensions) repo.

## Structure

Currently, Momor includes support for a number of languages without requiring installing an extension. Those languages can be found under [`crates/languages/src`](https://github.com/momor-industries/momor/tree/main/crates/languages/src).

Support for all other languages is done via extensions. This directory ([extensions/](https://github.com/momor-industries/momor/tree/main/extensions/)) contains some of the officially maintained extensions. These extensions use the same [momor_extension_api](https://docs.rs/momor_extension_api/latest/momor_extension_api/) available to all [Momor Extensions](https://momor.dev/extensions) for providing [language servers](https://momor.dev/docs/extensions/languages#language-servers), [tree-sitter grammars](https://momor.dev/docs/extensions/languages#grammar) and [tree-sitter queries](https://momor.dev/docs/extensions/languages#tree-sitter-queries).

You can find the other officially maintained extensions in the [momor-extensions organization](https://github.com/momor-extensions).

## Dev Extensions

See the docs for [Developing an Extension Locally](https://momor.dev/docs/extensions/developing-extensions#developing-an-extension-locally) for how to work with one of these extensions.
