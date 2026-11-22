# releasing

Releases build in CI on tag push — no local build, no secrets beyond the workflow's own `GITHUB_TOKEN`.

## Cut a release

1. Bump `version` in the root `Cargo.toml` (it is workspace-wide; every crate inherits it).
2. Refresh `Cargo.lock` (any cargo command rewrites it, e.g. `cargo update -w`).
3. Commit, push to `windows`, then tag and push the tag:

   ```
   git tag v<version>
   git push fork windows --tags
   ```

4. `.github/workflows/release.yml` runs: builds on `windows-latest` (Rust 1.99.1 and zig 0.15.2, both SHA-pinned), refuses a tag that does not match `rodeo`'s version, smoke-tests `rodeo --version`, packages `rodeo-x86_64-pc-windows-msvc.zip` (the executable alone at the archive root, plus a `.sha256`) and publishes a GitHub Release with generated notes. Re-running for the same tag re-uploads with `--clobber`; a `workflow_dispatch` with a `tag` input re-releases an existing tag.

## What this fork dropped from upstream's pipeline

No Homebrew tap (`rodeo.rb` template), no packslip signing/attestations, no Cloudflare R2 mirror, no `www/` site publishing — the fork's only distribution is GitHub Releases. `ci.yml` runs the workspace test suite on every push to `windows` and on PRs, with `herdr-core` 0.1.21 downloaded from `releases.herdr.dev` (SHA-pinned) so the hc-backed tests run against a real core.

## Local build

`./build.ps1` — `cargo build --release --locked -p rodeo` plus install to `~/.cargo/bin`. zig is only needed at build time (for `libghostty-vt-sys`); the script prepends a local zig if present.
