# TODO

- **The PTY integration suite fails locally and nothing runs it.** On 2026-10-12, `cargo test --test
  integration -- --test-threads=1` failed 5 of 10 at main (0.77.2) and 4 of 10 at 0.77.0, under a
  cleared environment too: the app does not react to keys (`e`, `c`, `?` draw nothing; the editor is
  never invoked). CI runs `--lib --bins` only, so the break went unseen. Find the commit or
  environment change that broke it, then run the suite in CI (quality-ratchets "Terminal UIs").
- **Replace refresh/head/500f_25c's `ci_median_ns` with the lowest of five CI runs.** The pin
  (6955900000) is the single first CI measurement, from run 43272055235. Once five CI runs have
  measured `refresh/head/500f_25c`, set it to the lowest of them (it may only go down, and stay
  within 6x its `ref_median_ns`), and record the run ids in the `benches/baselines.toml` header.
