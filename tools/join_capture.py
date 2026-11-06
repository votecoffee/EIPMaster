"""Validate and join a single Assembly Scanner capture (including rotated parts)."""
import argparse
from contextlib import nullcontext
import csv
from pathlib import Path, PureWindowsPath
import tomllib

METADATA = [
    "capture_start_utc", "elapsed_us", "sample_index", "connection_generation",
    "packet_sequence", "cip_sequence",
]


def read_header(part):
    with part.open(encoding="utf-8", newline="") as file:
        header = next(csv.reader(file, strict=True), [])
    if (header[:len(METADATA)] != METADATA or len(header) <= len(METADATA)
            or any(not name for name in header)
            or len(set(name.lower() for name in header)) != len(header)):
        raise ValueError(f"Unexpected header in {part}")
    return header


def rows(parts, header=None):
    if header is None:
        header = read_header(parts[0])
    start = None
    last_index = last_us = last_generation = -1
    for part in parts:
        with part.open(encoding="utf-8", newline="") as file:
            reader = csv.reader(file, strict=True)
            if next(reader, None) != header:
                raise ValueError(f"Unexpected header in {part}")
            for row in reader:
                if len(row) != len(header):
                    raise ValueError(f"Unexpected field count in {part}:{reader.line_num}")
                if start is None:
                    start = row[0]
                elapsed, index, generation, packet, cip = map(int, row[1:6])
                if (row[0] != start or index <= last_index or elapsed < last_us or
                        generation < last_generation or not 0 <= packet <= 0xFFFFFFFF or not 0 <= cip <= 0xFFFF):
                    raise ValueError(f"Wrong capture or row order in {part}:{reader.line_num}")
                last_index, last_us, last_generation = index, elapsed, generation
                yield row


def parts_for(prefix):
    result = sorted(prefix.parent.glob(f"{prefix.name}-*.csv"))
    if not result:
        raise ValueError(f"No files for capture prefix {prefix}")
    for number, path in enumerate(result, 1):
        if path.name != f"{prefix.name}-{number:06}.csv":
            raise ValueError(f"Missing or unrecognized capture part before {path}")
    return result


def counter(report, key):
    value = report.get(key)
    if type(value) is not int or value < 0:
        raise ValueError(f"Run report needs a nonnegative integer {key}")
    return value


def read_report(path, prefix, part_count):
    with path.open("rb") as file:
        report = tomllib.load(file)
    if report.get("mode") == "gui":
        report = report.get("capture")
        if not isinstance(report, dict):
            raise ValueError("GUI run report has no capture table")
    source = report.get("csv_prefix")
    if not isinstance(source, str) or PureWindowsPath(source).name != prefix.name:
        raise ValueError("Run report belongs to a different capture prefix")
    if "disk_error" in report:
        raise ValueError(f"Run report records a CSV writer error ({report['disk_error']})")
    seen, written, unchanged, overruns = (
        counter(report, key) for key in ("seen", "written", "unchanged", "buffer_overruns")
    )
    if seen != written + unchanged + overruns:
        raise ValueError("Run report has inconsistent CSV row counts")
    if "csv_parts" in report:
        expected = counter(report, "csv_parts")
        if expected != part_count:
            raise ValueError(f"Run report lists {expected} CSV parts, found {part_count}")
    return report


def transfer_checked(parts, header, report, writer):
    started = report.get("capture_start_utc", report.get("started_utc"))
    if started is not None and (not isinstance(started, str) or not started):
        raise ValueError("Run report has an invalid capture start timestamp")
    summaries = report.get("field_summaries", [])
    if not isinstance(summaries, list) or any(not isinstance(field, dict) for field in summaries):
        raise ValueError("Run report has invalid field summaries")
    if summaries and [field.get("name") for field in summaries] != header[len(METADATA):]:
        raise ValueError("CSV columns differ from the run report")
    first_values = [field.get("first_value") for field in summaries]
    last_values = [field.get("last_value") for field in summaries]
    count = generations = 0
    last_index = -1
    last_row = None
    generation = None
    for row in rows(parts, header):
        if started is not None and row[0] != started:
            raise ValueError("CSV start timestamp differs from the run report")
        if not count and summaries and row[len(METADATA):] != first_values:
            raise ValueError("First CSV values differ from the run report")
        count += 1
        last_index = int(row[2])
        last_row = row
        row_generation = int(row[3])
        if row_generation != generation:
            generations += 1
            generation = row_generation
        if writer is not None:
            writer.writerow(row)
    if count != report["written"]:
        raise ValueError(f"Run report lists {report['written']} written rows, found {count}")
    if summaries and (last_row is None or last_row[len(METADATA):] != last_values):
        raise ValueError("Last CSV values differ from the run report")
    missing_indexes = last_index + 1 - count
    trailing_overruns = report["buffer_overruns"] - missing_indexes
    if trailing_overruns < 0:
        raise ValueError(
            f"Found {missing_indexes} missing sample indexes but the run report lists "
            f"{report['buffer_overruns']} buffer overruns"
        )
    return (f"Verified the CSV capture. Saved row count {count}, part count {len(parts)}, "
            f"and logged generation count {generations}. "
            f"Sample-index gaps account for {missing_indexes} of {report['buffer_overruns']} reported overruns. "
            f"Overruns after the last saved row total {trailing_overruns}.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("capture_prefix", type=Path, help="Use the prefix shown in the app, without -000001.csv")
    parser.add_argument("output", type=Path, nargs="?", help="New joined CSV path")
    parser.add_argument("--report", type=Path, help="Verify against the capture.toml or GUI run report")
    args = parser.parse_args()
    if args.output is None and args.report is None:
        parser.error("provide a new joined CSV path or --report")
    output_created = False
    audit = None
    try:
        parts = parts_for(args.capture_prefix)
        header = read_header(parts[0])
        report = read_report(args.report, args.capture_prefix, len(parts)) if args.report else None
        destination = args.output.open("x", encoding="utf-8", newline="") if args.output else nullcontext()
        with destination as output:
            writer = None
            if output is not None:
                output_created = True
                writer = csv.writer(output, lineterminator="\r\n")
                writer.writerow(header)
            if report is None:
                writer.writerows(rows(parts, header))
            else:
                audit = transfer_checked(parts, header, report, writer)
    except (OSError, ValueError, csv.Error) as error:
        if output_created:
            args.output.unlink(missing_ok=True)
        parser.exit(1, f"{error}\n")
    if audit:
        print(audit)


if __name__ == "__main__":
    main()
