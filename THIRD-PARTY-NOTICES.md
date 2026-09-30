# Third-party notices

smugmug-cli's own code is MIT licensed. Its binaries are built from many
open-source crates; almost all are MIT, Apache-2.0, BSD or Zlib licensed
(run `cargo tree` for the full list). One is not:

## rawler (LGPL-2.1)

smugmug-cli uses [rawler](https://github.com/dnglab/dnglab/tree/main/rawler)
to convert RAW files that have no usable embedded preview into JPEGs.
rawler is licensed under the GNU Lesser General Public License, version 2.1;
the full text is in [`licenses/LGPL-2.1.txt`](licenses/LGPL-2.1.txt).

- rawler's source code: <https://github.com/dnglab/dnglab>, or the exact
  version used by a given smugmug-cli release via `cargo vendor`, pinned by
  that release's `Cargo.lock`. smugmug-cli does not modify rawler.
- smugmug-cli's complete source code is at
  <https://github.com/jhofker/smugmug-cli>. To use a modified rawler, add a
  `[patch.crates-io]` entry pointing `rawler` at your copy and rebuild with
  `cargo build --release`.
