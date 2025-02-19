# Unreleased

- Update ratatui(-image) dependencies
- Enable Ctrl+Z/Suspend functionality
- Rewrite with mupdf as the backend for much better performance and rendering quality

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
