# Releasing

1. Bump `version` in `Cargo.toml`, run `cargo build --locked` so `Cargo.lock`
   follows, and commit both.
2. Tag and push:

   ```sh
   git tag v0.1.0
   git push origin v0.1.0
   ```

   `.github/workflows/release.yml` builds five targets (Linux x86_64 and
   aarch64, macOS arm64 and x86_64, Windows x86_64), attaches a `.tar.gz` or
   `.zip` plus a `.sha256` for each, and drafts the release notes.

3. For the AUR, in `packaging/aur/`: set `pkgver` to the new version, run
   `updpkgsums` to replace the `SKIP` digest with the real one, then
   `makepkg --printsrcinfo > .SRCINFO` and push both files to the AUR repo.

`makepkg` runs the test suite during `check()`, so a broken build fails before
it reaches anyone.
