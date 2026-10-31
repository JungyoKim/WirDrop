# owly-awdl (vendored)

The AWDL node of [owly](https://github.com/t4t5/owly) v0.4.0 (commit
`2e9b2b0228432d5e402a9c90a031b419960d0bd1`), MIT, see `LICENSE`. The original overview is in
`UPSTREAM-README.md`.

This copy carries changes for Windows radios, under the same license:

- received frames carry a host timestamp (`Input::FrameAt`, `driver::Received::received_at`); the
  driver services due timers before traffic, polls both directions fairly, bounds its retry
  queues, and joins its thread when dropped;
- `Config` settings for A-MSDU, accepting a peer's Block Ack session (with A-MSDU and a window),
  VHT capabilities, the country code and an infrastructure channel;
- Block Ack as the originator (`Config::tx_addba`): an ADDBA request when bulk traffic starts,
  ADDBA response and DELBA handling, single packets as QoS data, and their counters in the
  driver's STATS line.

Test with `cargo test --manifest-path vendor/owly-awdl/Cargo.toml --features driver`.
