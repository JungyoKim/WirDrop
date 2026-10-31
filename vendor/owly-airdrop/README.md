# owly-airdrop (vendored)

The AirDrop protocol of [owly](https://github.com/t4t5/owly) v0.4.0 (commit
`2e9b2b0228432d5e402a9c90a031b419960d0bd1`), MIT, see `LICENSE`. The original overview is in
`UPSTREAM-README.md`.

This copy carries changes for Windows and current iPhones, under the same license:

- validated compression and HTTP completion before files are published, bounded upload
  progress, cancellable archive preparation, Windows no-replace publication with rollback, and
  an Apple ID identity (validation record, TLS chain, BLE hashes);
- several files from an iPhone: iOS wraps each item in a temporary folder
  (`./NSIRD_AirDrop_*/IMG_0001.PNG`); the receiver accepts such paths when the leaf is the
  offered name and saves the file under that name;
- the receiver driver ends a DVZip upload at the archive's end when iOS leaves the connection
  open without the last chunk (`Config::allow_dvzip_archive_boundary`, `tests/dvzip_boundary.rs`),
  logs each upload's compression, and reports an upload that stops as `Failed`.

Test with `cargo test --manifest-path vendor/owly-airdrop/Cargo.toml --lib --test dvzip_boundary --test vectors`.
