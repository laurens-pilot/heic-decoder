# HEIC Correctness and Performance Tests

This crate intentionally does not track image corpora, external validator source
trees, validator build products, or helper binaries. The test harness keeps all
generated files under `.heic-test-runs/`, and local assets under
`.heic-test-assets/`; both are gitignored.

The harness mirrors the correctness and performance checks used in `libheic-rs`.
It uses libheif only as an external validator and optional corpus source. The
crate does not use libheif source code or link to libheif.

The default corpus has two parts:

1. the libheif checkout's sample/test/fuzz images, and
2. the HEIC fixture corpus from
   <https://github.com/ente/test-fixtures> (`media/heic/v1/files`) — real
   camera files that have caught regressions the libheif corpus misses.

Changes to the HEIC decoder should be regression-tested with a default-corpus
`verify` run, which covers both. Neither corpus is ever checked into this
repository.

- pixel-for-pixel PNG comparison against an external `heif-dec` validator
- Rust decoder vs external validator decode timing
- bytes vs path ingestion timing
- `image` adapter vs direct decode timing
- path/read concurrent decode timing and RSS

## Setup

Put a libheif checkout or symlink under the ignored asset directory:

```bash
mkdir -p .heic-test-assets
ln -s /path/to/libheif .heic-test-assets/libheif
```

Cloning directly into `.heic-test-assets` is also accepted:

```bash
git clone https://github.com/strukturag/libheif.git .heic-test-assets
```

Or point the script at an existing validator/corpus checkout:

```bash
export HEIC_LIBHEIF_SOURCE_DIR=/path/to/libheif
```

The ente test-fixtures corpus needs no setup: the harness fetches it
automatically (a sparse, blobless clone of just the HEIC fixture subtree) into
`.heic-test-assets/ente-test-fixtures` the first time a default corpus is
assembled. If fetching is not possible (e.g. offline), it prints a warning and
runs with the libheif corpus only. To use a pre-existing checkout, clone it
yourself or point the script at it:

```bash
git clone https://github.com/ente/test-fixtures.git .heic-test-assets/ente-test-fixtures
# or
export HEIC_ENTE_FIXTURES_DIR=/path/to/test-fixtures
```

To pick up fixture files added upstream later, refresh the clone:

```bash
git -C .heic-test-assets/ente-test-fixtures pull
```

Then run:

```bash
scripts/heic_tests.sh all
```

The scripts can build the external validator into
`.heic-test-runs/validator-build` by default. Set
`LIBHEIF_DEC_BIN=/path/to/heif-dec` to reuse an existing validator binary
instead. The only auto-detected validator paths are under `.heic-test-assets/`
and `.heic-test-runs/`; explicit environment variables are left untouched.

Required command-line tools: `cargo`, `cmake`, `ffmpeg`, `ffprobe`, `shasum`,
`awk`, `find`, `sort`, and `/usr/bin/time`.

## Commands

Quick correctness pass:

```bash
scripts/heic_tests.sh verify --quick --require-exts heic,avif
```

Full correctness pass over the configured corpus:

```bash
scripts/heic_tests.sh verify --full --require-exts heic,avif
```

Performance checks:

```bash
scripts/heic_tests.sh bench-decode --full --files 12 --runs 5
scripts/heic_tests.sh bench-ingestion --full --files 12 --runs 5
scripts/heic_tests.sh bench-image --full --files 12 --runs 5
scripts/heic_tests.sh bench-stream --full --files 6 --runs 2 --workers 10 --iterations 4
```

Everything:

```bash
scripts/heic_tests.sh all
```

Passing `--corpus-dir` replaces the default corpus entirely — useful for
reproducing individual files. Default runs (no `--corpus-dir`) cover both the
libheif corpus and the ente fixtures.

Generated reports and PNG artifacts are under `.heic-test-runs/`. Use
`--keep-artifacts` with `verify` when debugging a pixel mismatch.
