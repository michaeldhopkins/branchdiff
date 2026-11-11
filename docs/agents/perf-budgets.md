# Performance budgets

`tests/perf.toml` is the manifest covering every discovered CLI command, TUI keystroke, HTML page and per-frame callback, gated by `cargo test --test perf_rules`. This file holds the rules in full; AGENTS.md has the summary.

## What may only tighten

Process-count floors (`processes`), allocation pins (`max_blocks`, `max_bytes`, `max_bytes_x86_64` in `benches/baselines.toml`) and stage floors may only tighten against the baseline revision (`PERF_BASELINE_REV` in CI, `main@origin` or `main` locally). A stage floor is one bench that runs. An operation with two or more running stages lists `stage_floors` (`name=us`, the bench ceiling) once per stage. `wall_floor_us` is the sum of those floors, not a floor of its own: adding a stage adds its floor and the total rises by that much, while every stage that already ran stays at its old floor or tighter. A single running stage keeps that floor in `wall_floor_us` and does not repeat it. A target may rise only when a new stage raised the total, and only by as much as the total rose. When a change reduces spawns, allocations or bench medians, tighten the matching entries so the guard locks in the win. An entry whose `wall_floor_us` exceeds its class cap carries a one-paragraph `proposal` naming where the time goes and the batched structure that removes it.

A keystroke that has its own bench must bind it: `keystrokes_with_their_own_bench_are_bound_to_it` (`tests/perf_rules.rs`) fails when a `[[bench]]` named `navigation/<key>` exists but the key's op does not list it in `benches`.

## Floors

A floor sums every stage that runs in sequence. `alternatives` are only other fixtures of a stage already counted: they replace that stage and are not added.

- `floor = "sum"` is the sum of the benches that are not in `alternatives`.
- `floor = "max"` is one stage. Every other bench is an alternative. No percentage drops a bench.
- An alternative cannot tie the largest bench. Equal stages are summed.
- Duplicate alternative names are rejected, and every alternative must be one of the op's benches.
- On a sum, an alternative must be strictly smaller than at least one stage that runs. A fixture at least as large as every stage belongs in the sum.
- Paying off an owed operation may introduce alternatives. A measured operation may not add one.
- Two or more running stages require `stage_floors`. Each entry is `bench=us` and `us` is that bench's ceiling. Alternatives are not stage floors. The numbers only tighten against the baseline stage of the same bench. A bench the baseline did not run is a new stage.

The checks are `check_stage_floor` and `require_stage_floors` in `tests/perf_rules/guard.rs`.

## Wall medians are checked in CI

The performance bench leaves Criterion data under `target/criterion`. CI then runs `cargo test --test wall_medians` as a blocking step; locally set `PERF_WALL_CHECK=1` after a full bench run.

The pins come from one reference Mac and a CI runner is a different machine, so absolute times are not compared. Each bench's measured/pinned ratio is divided by the median ratio across all benches (the machine's overall speed), and a bench whose normalised ratio passes 2.0 (100% tolerance) fails, unless it is also over by less than 10 ns (a few-nanosecond bench swings by its own size between CPUs). Consequences:

- lowering a `ref_median_ns` pin without a measurement that supports it fails;
- a bench with a pin and no measurement fails, as does a pin of zero;
- a uniform slowdown moves every ratio and the median together and is not caught, by design.

Fewer than 8 benches make the median a single bench rather than a machine speed, so the check refuses to run.

## Adding a bench

Measure it with `black_box` around the operation (an unobserved call was folded away to 0.43 ns), add the `[[bench]]` entry to `benches/baselines.toml` with `ref_median_ns` (above zero) and the allocation counts from the alloc bench, then bind it in `tests/perf.toml` with `wall_floor_us` equal to the bench's ceiling. Run the benches in the heavy slot.
