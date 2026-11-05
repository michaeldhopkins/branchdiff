# Performance budgets

`tests/perf.toml` is the manifest covering every discovered CLI command, TUI keystroke, HTML page and per-frame callback, gated by `cargo test --test perf_rules`. This file holds the rules in full; AGENTS.md has the summary.

## What may only tighten

Process-count floors (`processes`), allocation pins (`max_blocks`, `max_bytes`, `max_bytes_x86_64` in `benches/baselines.toml`), wall floors (`wall_floor_us`, equal to the ceiling of the `ref_median_ns` of its `benches`, or `owed = true` while unbenched) and targets (`target_us`) may only tighten against the baseline revision (`PERF_BASELINE_REV` in CI, `main@origin` or `main` locally). When a change reduces spawns, allocations or bench medians, tighten the matching entries so the guard locks in the win. An entry whose `wall_floor_us` exceeds its class cap carries a one-paragraph `proposal` naming where the time goes and the batched structure that removes it.

A keystroke that has its own bench must bind it: `keystrokes_with_their_own_bench_are_bound_to_it` (`tests/perf_rules.rs`) fails when a `[[bench]]` named `navigation/<key>` exists but the key's op does not list it in `benches`.

## `floor = "max"` and `alternatives`

A max floor over several benches discards every bench but the largest. That is honest only when the others are negligible or are other fixtures of the same stage:

- the other benches together add at most 10% of the dominant bench, or
- the op declares them in `alternatives = [...]`: benches that are alternative fixtures (50-file against 500-file repo), not stages that run one after another.

Rules (`check_max_floor` in `tests/perf_rules/guard.rs`): `alternatives` needs `floor = "max"`; each entry must be one of the op's `benches`; the dominant bench can never be an alternative; and the list may not grow against the baseline, so a regression cannot be hidden by declaring its bench an alternative. If the stages run in sequence, use `floor = "sum"`.

## Wall medians are checked in CI

The performance bench leaves Criterion data under `target/criterion`. CI then runs `cargo test --test wall_medians` as a blocking step; locally set `PERF_WALL_CHECK=1` after a full bench run.

The pins come from one reference Mac and a CI runner is a different machine, so absolute times are not compared. Each bench's measured/pinned ratio is divided by the median ratio across all benches (the machine's overall speed), and a bench whose normalised ratio passes 2.0 (100% tolerance) fails, unless it is also over by less than 10 ns (a few-nanosecond bench swings by its own size between CPUs). Consequences:

- lowering a `ref_median_ns` pin without a measurement that supports it fails;
- a bench with a pin and no measurement fails, as does a pin of zero;
- a uniform slowdown moves every ratio and the median together and is not caught, by design.

Fewer than 8 benches make the median a single bench rather than a machine speed, so the check refuses to run.

## Adding a bench

Measure it with `black_box` around the operation (an unobserved call was folded away to 0.43 ns), add the `[[bench]]` entry to `benches/baselines.toml` with `ref_median_ns` (above zero) and the allocation counts from the alloc bench, then bind it in `tests/perf.toml` with `wall_floor_us` equal to the bench's ceiling. Run the benches in the heavy slot.
