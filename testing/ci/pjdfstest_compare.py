#!/usr/bin/env python3
"""Compare a pjdfstest ``prove -v`` log with the list of known failures.

Every subtest is identified as ``<dir>/<NN>.t:<number>``, e.g. ``chmod/00.t:117``.
The run fails when

* a subtest fails that is not listed as a known failure, or
* a test file did not run all of its planned subtests (crash, hang, timeout).

Known failures that pass now are reported so the list can be trimmed, but they
do not fail the run. Results marked ``# TODO`` or ``# SKIP`` are not failures.

Known-failures file format: one ``<file>:<numbers>`` entry per line, where
``<numbers>`` is a comma-separated list of subtest numbers or ranges (``1-4``).
Everything after ``#`` is a comment.
"""

import argparse
import os
import re
import sys

FILE_RE = re.compile(r"\btests/([A-Za-z0-9_]+/[0-9]+\.t)\b")
PLAN_RE = re.compile(r"^1\.\.([0-9]+)\b")
RESULT_RE = re.compile(r"^(ok|not ok) ([0-9]+)(?=\s|$)(.*)$")
MAX_ANNOTATIONS = 50
MAX_DESCRIPTION = 200


class TestFile:
    def __init__(self, name):
        self.name = name
        self.planned = None
        self.ran = set()
        self.failed = {}  # subtest number -> description


def parse_log(path):
    files = {}
    order = []
    current = None
    with open(path, encoding="utf-8", errors="replace") as log:
        for raw in log:
            line = raw.rstrip("\n")
            result = RESULT_RE.match(line)
            if result is None:
                header = FILE_RE.search(line)
                if header:
                    name = header.group(1)
                    if name not in files:
                        files[name] = TestFile(name)
                        order.append(name)
                    current = files[name]
                    continue
                plan = PLAN_RE.match(line)
                if plan and current is not None and current.planned is None:
                    current.planned = int(plan.group(1))
                continue
            if current is None:
                continue
            status, number, rest = result.group(1), int(result.group(2)), result.group(3)
            current.ran.add(number)
            directive = rest.split("#", 1)[1].strip().upper() if "#" in rest else ""
            if status == "not ok" and not directive.startswith(("TODO", "SKIP")):
                current.failed[number] = shorten(rest.strip(" -"))
    return [files[name] for name in order]


def shorten(text):
    """pjdfstest's PATH_MAX tests print paths of several kilobytes."""
    if len(text) <= MAX_DESCRIPTION:
        return text
    return text[: MAX_DESCRIPTION - 3] + "..."


def annotation(text):
    """Escape a message for a GitHub Actions workflow command."""
    return text.replace("%", "%25").replace("\r", "%0D").replace("\n", "%0A")


def parse_numbers(spec):
    numbers = set()
    for part in spec.split(","):
        part = part.strip()
        if not part:
            continue
        low, _, high = part.partition("-")
        numbers.update(range(int(low), int(high or low) + 1))
    return numbers


def load_known(path):
    known = {}
    with open(path, encoding="utf-8") as listing:
        for lineno, raw in enumerate(listing, 1):
            entry = raw.split("#", 1)[0].strip()
            if not entry:
                continue
            name, sep, spec = entry.partition(":")
            if not sep or not spec:
                sys.exit(f"{path}:{lineno}: expected '<dir>/<NN>.t:<numbers>', got {entry!r}")
            known.setdefault(name.strip(), set()).update(parse_numbers(spec))
    return known


def compact(numbers):
    """Render sorted subtest numbers as ranges: [1, 2, 3, 5] -> '1-3,5'."""
    ranges = []
    for number in sorted(numbers):
        if ranges and number == ranges[-1][1] + 1:
            ranges[-1][1] = number
        else:
            ranges.append([number, number])
    return ",".join(str(a) if a == b else f"{a}-{b}" for a, b in ranges)


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("log", help="output of `prove -v` over pjdfstest")
    parser.add_argument("known", help="known-failures file")
    parser.add_argument(
        "--emit-known",
        action="store_true",
        help="print the failures of this run in known-failures format and exit",
    )
    args = parser.parse_args()

    files = parse_log(args.log)
    if not files:
        print(f"error: no pjdfstest results found in {args.log}")
        return 1

    if args.emit_known:
        for test_file in sorted(files, key=lambda f: f.name):
            if test_file.failed:
                print(f"{test_file.name}:{compact(test_file.failed)}")
        return 0

    known = load_known(args.known)
    new, fixed, incomplete = [], [], []
    ran = failed = 0
    for test_file in files:
        expected = known.get(test_file.name, set())
        ran += len(test_file.ran)
        failed += len(test_file.failed)
        for number in sorted(test_file.failed):
            if number not in expected:
                new.append((test_file.name, number, test_file.failed[number]))
        fixed.extend(
            (test_file.name, number)
            for number in sorted(expected)
            if number in test_file.ran and number not in test_file.failed
        )
        if test_file.planned is None or len(test_file.ran) < test_file.planned:
            incomplete.append(test_file)

    print(f"pjdfstest: {len(files)} files, {ran} subtests, {ran - failed} passed, {failed} failed")
    print(f"  known failures still failing: {failed - len(new)}")
    print(f"  new failures:                 {len(new)}")
    print(f"  known failures now passing:   {len(fixed)}")
    print(f"  incomplete test files:        {len(incomplete)}")

    in_actions = os.environ.get("GITHUB_ACTIONS") == "true"
    for name, number, description in new:
        print(f"NEW FAILURE {name}:{number} {description}")
    for test_file in incomplete:
        planned = "?" if test_file.planned is None else test_file.planned
        print(f"INCOMPLETE {test_file.name}: ran {len(test_file.ran)} of {planned} subtests")
    for name, number in fixed:
        print(f"NOW PASSING {name}:{number} (remove it from the known-failures list)")
    if in_actions:
        for name, number, description in new[:MAX_ANNOTATIONS]:
            print(f"::error title=pjdfstest {name}:{number}::{annotation(description)}")
        for test_file in incomplete:
            print(f"::error title=pjdfstest {test_file.name}::did not run all planned subtests")
        if fixed:
            print(f"::notice title=pjdfstest::{len(fixed)} known failures pass now")

    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a", encoding="utf-8") as out:
            out.write("### pjdfstest over NFSv3\n\n")
            out.write("| files | subtests | passed | known failures | new failures | now passing |")
            out.write(" incomplete |\n|---|---|---|---|---|---|---|\n")
            out.write(
                f"| {len(files)} | {ran} | {ran - failed} | {failed - len(new)} | {len(new)} |"
                f" {len(fixed)} | {len(incomplete)} |\n\n"
            )
            for name, number, description in new[:MAX_ANNOTATIONS]:
                out.write(f"- new failure `{name}:{number}`: {description}\n")

    return 1 if new or incomplete else 0


if __name__ == "__main__":
    sys.exit(main())
