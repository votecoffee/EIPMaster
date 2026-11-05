"""Validate and join a single Assembly Scanner capture (including rotated parts)."""
import argparse
import csv
from pathlib import Path

HEADER = [
    "capture_start_utc", "elapsed_us", "sample_index", "connection_generation",
    "packet_sequence", "cip_sequence", "value",
]


def rows(parts):
    start = None
    last_index = last_us = last_generation = -1
    for part in parts:
        with part.open(encoding="utf-8", newline="") as file:
            reader = csv.reader(file, strict=True)
            if next(reader, None) != HEADER:
                raise ValueError(f"Unexpected header in {part}")
            for row in reader:
                if len(row) != len(HEADER):
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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("capture_prefix", type=Path, help="Use the prefix shown in the app, without -000001.csv")
    parser.add_argument("output", type=Path, help="New joined CSV path")
    args = parser.parse_args()
    parts = parts_for(args.capture_prefix)
    output_created = False
    try:
        with args.output.open("x", encoding="utf-8", newline="") as output:
            output_created = True
            writer = csv.writer(output, lineterminator="\r\n")
            writer.writerow(HEADER)
            writer.writerows(rows(parts))
    except (OSError, ValueError, csv.Error):
        if output_created:
            args.output.unlink(missing_ok=True)
        raise


if __name__ == "__main__":
    main()
