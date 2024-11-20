# Unreleased

- Add `--r-to-l` flag to support displaying pdfs that read from right to left
- Add `--max-wide` flag to restrict amount of pages that can appear on the screen at a time
- Small internal changes to accomodate a few more clippy lints
- Update `ratatui` and `ratatui-image` git dependencies to latest upstream
- Move `ratatui-image/vb64` support under `nightly` feature, enabled by default
- Fixed a bug where jumping to a page out of range could result in weird `esc` key behavior
- Added CI

# v0.1.0

Initial tag :)
