#!/usr/bin/env python3
"""
Profile branchdiff and generate a human-readable report.

Usage:
    ./scripts/profile.py [--frames N] [--repo PATH]

Requires: samply (cargo install samply)
"""

import argparse
import json
import os
import subprocess
import sys
import tempfile
from collections import defaultdict
from pathlib import Path


def categorize_function(name: str) -> tuple[str, str]:
    """Categorize a function by its source and return (category, color)."""
    if name.startswith("branchdiff::"):
        return ("branchdiff", "\033[92m")  # Green
    elif name.startswith("ratatui::"):
        return ("ratatui", "\033[93m")  # Yellow
    elif name.startswith("crossterm::"):
        return ("crossterm", "\033[93m")  # Yellow
    elif name.startswith("similar::"):
        return ("similar", "\033[93m")  # Yellow
    elif name.startswith("unicode_segmentation::") or name.startswith("unicode_width::"):
        return ("unicode", "\033[93m")  # Yellow
    elif name.startswith("std::") or name.startswith("core::") or name.startswith("alloc::"):
        return ("std", "\033[94m")  # Blue
    elif name.startswith("<") and "Iterator" in name:
        return ("std", "\033[94m")  # Blue
    elif name.startswith("_") or name in ("poll", "main", "start"):
        return ("system", "\033[90m")  # Gray
    elif "::" in name:
        # Try to extract crate name
        crate = name.split("::")[0]
        if crate in ("hashbrown", "compact_str", "foldhash"):
            return ("std", "\033[94m")
        return ("deps", "\033[93m")  # Yellow
    else:
        return ("system", "\033[90m")  # Gray


def parse_profile(profile_path: str, syms_path: str) -> dict:
    """Parse samply profile and symbols files."""
    with open(profile_path) as f:
        profile = json.load(f)
    with open(syms_path) as f:
        syms = json.load(f)

    sym_string_table = syms["string_table"]
    thread = profile["threads"][0]
    func_table = thread["funcTable"]
    frame_table = thread["frameTable"]
    samples = thread.get("samples", {})
    string_array = thread.get("stringArray", [])
    resource_table = thread.get("resourceTable", {})
    libs = profile.get("libs", [])
    lib_names = [lib.get("name", "") for lib in libs]

    # Build symbol lookup
    func_names = []
    for i in range(len(func_table.get("name", []))):
        name_idx = func_table["name"][i]
        if name_idx < len(string_array):
            raw_name = string_array[name_idx]
            if raw_name.startswith("0x"):
                resource_idx = (
                    func_table.get("resource", [])[i]
                    if i < len(func_table.get("resource", []))
                    else -1
                )
                if resource_idx >= 0 and resource_idx < len(
                    resource_table.get("lib", [])
                ):
                    lib_idx = resource_table["lib"][resource_idx]
                    if lib_idx < len(lib_names):
                        lib_name = lib_names[lib_idx].split("/")[-1]
                        for lib_data in syms["data"]:
                            if lib_data.get("debug_name", "") == lib_name:
                                addr = int(raw_name, 16)
                                for entry in lib_data.get("symbol_table", []):
                                    if (
                                        entry["rva"]
                                        <= addr
                                        < entry["rva"] + entry.get("size", 1)
                                    ):
                                        sym_idx = entry.get("symbol", 0)
                                        if sym_idx < len(sym_string_table):
                                            raw_name = sym_string_table[sym_idx]
                                        break
                                break
            func_names.append(raw_name)
        else:
            func_names.append(f"<unknown_{i}>")

    # Count samples
    frame_func = frame_table.get("func", [])
    sample_stacks = samples.get("stack", [])
    stack_table = thread.get("stackTable", {})
    stack_frame = stack_table.get("frame", [])
    stack_prefix = stack_table.get("prefix", [])

    func_counts = defaultdict(int)
    self_counts = defaultdict(int)
    total_samples = len(sample_stacks)

    for stack_idx in sample_stacks:
        if stack_idx is None:
            continue
        is_leaf = True
        seen = set()
        while stack_idx is not None and stack_idx not in seen:
            seen.add(stack_idx)
            if stack_idx < len(stack_frame):
                frame_idx = stack_frame[stack_idx]
                if frame_idx < len(frame_func):
                    func_idx = frame_func[frame_idx]
                    if func_idx < len(func_names):
                        name = func_names[func_idx]
                        func_counts[name] += 1
                        if is_leaf:
                            self_counts[name] += 1
                            is_leaf = False
            if stack_idx < len(stack_prefix):
                stack_idx = stack_prefix[stack_idx]
            else:
                break

    return {
        "total_samples": total_samples,
        "self_counts": dict(self_counts),
        "inclusive_counts": dict(func_counts),
    }


def print_report(data: dict, use_color: bool = True):
    """Print formatted profiling report."""
    total = data["total_samples"]
    self_counts = data["self_counts"]
    inclusive_counts = data["inclusive_counts"]

    reset = "\033[0m" if use_color else ""
    bold = "\033[1m" if use_color else ""
    dim = "\033[2m" if use_color else ""

    print(f"\n{bold}Total samples: {total}{reset}\n")

    # Self time report
    print(f"{bold}{'='*90}")
    print("SELF TIME - Where CPU is actually spending time")
    print(f"{'='*90}{reset}\n")
    print(f"{'%':>6}  {'Count':>5}  {'Source':<12}  Function")
    print(f"{'-'*6}  {'-'*5}  {'-'*12}  {'-'*60}")

    sorted_self = sorted(self_counts.items(), key=lambda x: -x[1])
    shown = 0
    category_totals = defaultdict(float)

    for name, count in sorted_self:
        if shown >= 30:
            break
        if name.startswith("0x") or name == "" or name == "UNKNOWN":
            continue

        pct = 100.0 * count / total if total > 0 else 0
        category, color = categorize_function(name)
        category_totals[category] += pct

        if not use_color:
            color = ""

        display = name
        if len(display) > 55:
            display = display[:52] + "..."

        print(f"{pct:5.1f}%  {count:5d}  {color}{category:<12}{reset}  {display}")
        shown += 1

    # Category summary
    print(f"\n{bold}Self-time by source:{reset}")
    for cat in ["branchdiff", "ratatui", "std", "system", "deps", "unicode"]:
        if cat in category_totals:
            cat_color = categorize_function(f"{cat}::x")[1] if use_color else ""
            print(f"  {cat_color}{cat:<12}{reset}: {category_totals[cat]:5.1f}%")

    # Inclusive time report (shorter)
    print(f"\n{bold}{'='*90}")
    print("INCLUSIVE TIME - Time in function + all functions it calls")
    print(f"{'='*90}{reset}\n")
    print(f"{'%':>6}  {'Source':<12}  Function")
    print(f"{'-'*6}  {'-'*12}  {'-'*65}")

    sorted_inc = sorted(inclusive_counts.items(), key=lambda x: -x[1])
    shown = 0
    for name, count in sorted_inc:
        if shown >= 20:
            break
        if name.startswith("0x") or name == "" or name == "UNKNOWN":
            continue
        # Skip the obvious top-level functions
        if name in ("main", "start", "std::rt::lang_start_internal"):
            continue

        pct = 100.0 * count / total if total > 0 else 0
        category, color = categorize_function(name)
        if not use_color:
            color = ""

        display = name
        if len(display) > 60:
            display = display[:57] + "..."

        print(f"{pct:5.1f}%  {color}{category:<12}{reset}  {display}")
        shown += 1

    # Actionable insights
    print(f"\n{bold}{'='*90}")
    print("OPTIMIZATION GUIDANCE")
    print(f"{'='*90}{reset}\n")

    branchdiff_pct = category_totals.get("branchdiff", 0)
    ratatui_pct = category_totals.get("ratatui", 0)
    std_pct = category_totals.get("std", 0)

    print(f"{dim}Source categories:{reset}")
    print(f"  {bold}branchdiff{reset} - Our code, directly optimizable")
    print(f"  {bold}ratatui/deps{reset} - Dependencies, optimize by reducing calls or changing usage")
    print(f"  {bold}std{reset} - Standard library, optimize by using different data structures")
    print(f"  {bold}system{reset} - OS/runtime, generally unavoidable\n")

    if branchdiff_pct < 5:
        print(f"  {dim}[OK]{reset} branchdiff code is efficient ({branchdiff_pct:.1f}% self-time)")
    else:
        print(f"  {bold}[!]{reset} branchdiff code uses {branchdiff_pct:.1f}% - check hot functions above")

    if ratatui_pct > 30:
        print(f"  {bold}[!]{reset} ratatui uses {ratatui_pct:.1f}% - consider reducing widgets/cells rendered")

    print()


def main():
    parser = argparse.ArgumentParser(description="Profile branchdiff")
    parser.add_argument("--frames", type=int, default=5000, help="Number of frames to render")
    parser.add_argument("--repo", type=str, default=".", help="Repository path to profile")
    parser.add_argument("--no-color", action="store_true", help="Disable colored output")
    args = parser.parse_args()

    # Check for samply
    if subprocess.run(["which", "samply"], capture_output=True).returncode != 0:
        print("Error: samply not found. Install with: cargo install samply", file=sys.stderr)
        sys.exit(1)

    # Find branchdiff binary
    script_dir = Path(__file__).parent
    project_dir = script_dir.parent
    binary = project_dir / "target" / "profiling" / "branchdiff"

    if not binary.exists():
        print("Building with profiling profile...")
        subprocess.run(
            ["cargo", "build", "--profile", "profiling"],
            cwd=project_dir,
            check=True,
        )

    # Run samply
    with tempfile.TemporaryDirectory() as tmpdir:
        profile_path = os.path.join(tmpdir, "profile.json")
        syms_path = os.path.join(tmpdir, "profile.syms.json")

        print(f"Profiling {args.frames} frames...")
        result = subprocess.run(
            [
                "samply",
                "record",
                "--save-only",
                "--unstable-presymbolicate",
                "-o",
                profile_path,
                "--",
                str(binary),
                "--benchmark",
                str(args.frames),
                args.repo,
            ],
            capture_output=True,
            text=True,
        )

        # Print benchmark output
        if result.stderr:
            for line in result.stderr.strip().split("\n"):
                if line.startswith("Loading") or line.startswith("Loaded") or line.startswith("Running") or line.startswith("Results") or line.startswith("  "):
                    print(line)

        if not os.path.exists(profile_path) or not os.path.exists(syms_path):
            print(f"Error: Profile files not created", file=sys.stderr)
            print(f"samply stderr: {result.stderr}", file=sys.stderr)
            sys.exit(1)

        # Parse and report
        data = parse_profile(profile_path, syms_path)
        print_report(data, use_color=not args.no_color)


if __name__ == "__main__":
    main()
