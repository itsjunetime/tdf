# Unreleased

- Switched simd base64 crate for one that works on stable (from `vb64` to `base64_simd`)
- Allow boolean arguments to function as flags, without a `true` or `false` argument following the flag itself ([#109](https://github.com/itsjunetime/tdf/pull/109), thanks [@tatounee](https://github.com/tatounee)!)
- Fix cropping issues when zooming out too much while using kitty protocol
- Added `gg` and `G` keybindings for scrolling to the top and bottom of a page, respectively, when filling the width of the screen with kitty
- Updated help page to only show kitty keybindings when you're actually using kitty
- Map page-up and page-down keybindings to do the same thing as up-key and down-key ([#115](https://github.com/itsjunetime/tdf/pull/115), thanks [@maxdexh](https://github.com/maxdexh)!)
- Vertically center pages within the available space if they are not constrained by the height ([#116](https://github.com/itsjunetime/tdf/pull/116), thanks [@maxdexh](https://github.com/maxdexh)!)
- Fixed issue with cooked mode not being restored upon panic/error ([#118](https://github.com/itsjunetime/tdf/pull/118), thanks [@maxdexh](https://github.com/maxdexh)!)
- Implemented a debounce for file reload updates to prevent some editors from paralyzing the app due to a flurry of reloads ([#117](https://github.com/itsjunetime/tdf/pull/117), thanks [@maxdexh](https://github.com/maxdexh))
- Fixed an overflow when zooming out of horizontal pdfs ([#119](https://github.com/itsjunetime/tdf/pull/119), thanks [@maxdexh](https://github.com/maxdexh)!)
- Reworked zooming to allow for full zooming in and out and panning in both directions ([#121](https://github.com/itsjunetime/tdf/pull/121), thanks [@maxdexh](https://github.com/maxdexh)!)

# v0.4.3

- Fix issue with some terminals hanging on startup
- Fix issues with some iterm2-backend terminals not displaying anything
- Allow using ctrl+scroll to zoom in/out while zoomed using kitty backend
- (Internal) run CI with `--locked` flag to ensure lockfile is always in-sync

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
