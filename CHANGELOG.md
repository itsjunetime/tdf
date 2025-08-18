# Unreleased

# v0.4.2

- Add `--version` flag
- Fix shms not working on macos ([#93](https://github.com/itsjunetime/tdf/pull/93))

# v0.4.1

- Add instructions for using new zoom/pan features to help page

# v0.4.0

- Update to new `kittage` backend for kitty-protocol-supporting terminals (fixes many issues and improves performance significantly, see [the PR](https://github.com/itsjunetime/tdf/pull/74))
- Use new mupdf search API for slightly better performance
- Update ratatui(-image) dependencies
- Allow specification of default white and black colors for rendered pdfs
- Pause rendering every once in a while while there's a search term to enable searching across the entire document more quickly
- Fix an issue with missing search highlights

# v0.3.0

- Update ratatui(-image) dependencies
- Enable Ctrl+Z/Suspend functionality
- Rewrite with mupdf as the backend for much better performance and rendering quality
- Support easy inversion of colors via `i` keypress
- Support for filling all available space with `f` keypress
- Change help text at bottom into full help page

# v0.2.0

- Add `--r-to-l` flag to support displaying pdfs that read from right to left
- Add `--max-wide` flag to restrict amount of pages that can appear on the screen at a time
- Small internal changes to accomodate a few more clippy lints
- Update `ratatui` and `ratatui-image` git dependencies to latest upstream
- Move `ratatui-image/vb64` support under `nightly` feature, enabled by default
- Fixed a bug where jumping to a page out of range could result in weird `esc` key behavior
- Added CI ([#31](https://github.com/itsjunetime/tdf/pull/31), thank you [@Kriejstal](https://github.com/Kreijstal))
- Changed global allocator to [`mimalloc`](https://github.com/purpleprotocol/mimalloc_rust) for slightly improved performance
- Fixed issue with document reloading not working when files are intermedially deleted
- Fixed a lot of weirdness with bottom message layering/updating

# v0.1.0

Initial tag :)
