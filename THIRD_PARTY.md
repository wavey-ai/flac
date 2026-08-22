# Third-party provenance

Wavey FLAC intentionally started from two established Apache-2.0 Rust
implementations instead of rewriting the format from scratch.

## flacenc-rs

- Upstream: <https://github.com/yotarok/flacenc-rs>
- Role: encoder foundation
- License: Apache-2.0
- Preserved as the first-parent history of this repository
- Original documentation: [`docs/FLACENC.md`](docs/FLACENC.md)

Copyright notices in the original files are retained.

## Claxon

- Upstream: <https://github.com/ruuda/claxon>
- Role: decoder foundation
- License: Apache-2.0
- Preserved as the second-parent history merged into this repository
- Original license: [`LICENSE-CLAXON`](LICENSE-CLAXON)
- Original documentation: [`docs/CLAXON.md`](docs/CLAXON.md)
- Original changelog: [`docs/CLAXON_CHANGELOG.md`](docs/CLAXON_CHANGELOG.md)

The decoder source files retain their original Claxon copyright and license
headers. Wavey-specific modifications are reviewable as commits after the
history merge.
