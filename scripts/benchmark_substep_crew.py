#!/usr/bin/env python3
"""Focused, non-asserting microbenchmarks for substep_crew optimizations.

Reports tracemalloc Python allocation peaks separately from process-wide ru_maxrss.
Algorithm timings are isolated microbenchmarks, not end-to-end harness timings.
"""

import contextlib
import difflib
import importlib.util
import os
from pathlib import Path
import resource
import sys
import tempfile
import time
import tracemalloc

SPEC = importlib.util.spec_from_file_location("substep_crew", Path(__file__).with_name("substep_crew.py"))
crew = importlib.util.module_from_spec(SPEC)
assert SPEC.loader
sys.modules[SPEC.name] = crew
SPEC.loader.exec_module(crew)


def measure(label, function, repeats=1):
    tracemalloc.start()
    started = time.perf_counter()
    for _ in range(repeats):
        function()
    elapsed = (time.perf_counter() - started) / repeats
    _, peak = tracemalloc.get_traced_memory()
    tracemalloc.stop()
    rss_kib = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    print(f"{label:40} {elapsed*1000:9.2f} ms  py_peak={peak/1024:9.1f} KiB  process_peak_rss={rss_kib} KiB")


def transcript_case(size):
    def run():
        with tempfile.TemporaryDirectory() as td, open(os.devnull, "w") as sink, contextlib.redirect_stdout(sink):
            result = crew._stream(
                [sys.executable, "-c", f"import os; b=b'x'*16384; n={size}; [os.write(1,b[:min(len(b),n-i)]) for i in range(0,n,len(b))]"],
                30,
                "bench",
                log_path=Path(td) / "full.log",
            )
            assert len(result.output) < 6000
    return run


def old_guide_lines(body, offsets):
    return [body.count("\n", 0, offset) + 1 for offset in offsets]


def new_guide_lines(body, offsets):
    line, previous, result = 1, 0, []
    for offset in offsets:
        line += body.count("\n", previous, offset)
        previous = offset
        result.append(line)
    return result


def old_match(previous, current, threshold=.72):
    result = []
    for c in current["blockers"]:
        cp = c["problem"].lower()
        for p in previous["blockers"]:
            if p["file"] == c["file"] and difflib.SequenceMatcher(None, cp, p["problem"].lower()).ratio() >= threshold:
                result.append(c["problem"])
                break
    return result


def indexed_filtered_match(previous, current, threshold=.72):
    by_file = {}
    for blocker in previous["blockers"]:
        by_file.setdefault(blocker["file"], []).append(blocker["problem"].lower())
    result = []
    for blocker in current["blockers"]:
        current_problem = blocker["problem"].lower()
        for prior in by_file.get(blocker["file"], ()):
            matcher = difflib.SequenceMatcher(None, current_problem, prior)
            if matcher.real_quick_ratio() < threshold or matcher.quick_ratio() < threshold:
                continue
            if matcher.ratio() >= threshold:
                result.append(blocker["problem"])
                break
    return result


def matching_case(count, exact=False):
    previous = {"blockers": [{"file": f"f{i%12}.py", "problem": ("validate tenant ownership before creating resource " + str(i))} for i in range(count)]}
    current = {"blockers": [{"file": f"f{i%12}.py", "problem": previous["blockers"][i]["problem"] if exact else ("validate tenant owner before resource creation " + str(i))} for i in range(count)]}
    return previous, current


def main():
    print("Transcript capture (subprocess + disk I/O; retained excerpt is bounded):")
    measure("transcript 64 KiB", transcript_case(64 * 1024))
    measure("transcript 16 MiB", transcript_case(16 * 1024 * 1024))

    print("\nGuide heading line counting (isolated algorithm):")
    for headings in (20, 4000):
        section = "body line\n" * 20
        body = "prefix\n" + section * headings
        offsets = [len("prefix\n") + len(section) * i for i in range(headings)]
        measure(f"old prefix scans ({headings})", lambda b=body,o=offsets: old_guide_lines(b,o), repeats=5)
        measure(f"incremental scans ({headings})", lambda b=body,o=offsets: new_guide_lines(b,o), repeats=5)

    print("\nBlocker matching (isolated algorithm; indexed version returns formatted strings):")
    for count in (8, 600):
        previous, current = matching_case(count)
        measure(f"old nested fuzzy ({count})", lambda p=previous,c=current: old_match(p,c), repeats=3)
        measure(f"indexed fuzzy ({count})", lambda p=previous,c=current: crew.repeated_blockers(p,c), repeats=3)
        measure(f"indexed + upper bounds ({count})", lambda p=previous,c=current: indexed_filtered_match(p,c), repeats=3)
        exact_previous, exact_current = matching_case(count, exact=True)
        measure(f"indexed exact fast path ({count})", lambda p=exact_previous,c=exact_current: crew.repeated_blockers(p,c), repeats=3)

    print("\nNotes: ru_maxrss is cumulative whole-process peak (Linux KiB), so later rows cannot decrease. "
          "tracemalloc covers Python allocations, not child-process or OS pipe buffers. Small inputs may "
          "show indexing/filter overhead; timings are not correctness assertions.")


if __name__ == "__main__":
    main()
