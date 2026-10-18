"""Command-line interface."""

import argparse

from openconverter import __version__


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="openconverter", description="Offline file converter.")
    parser.add_argument("--version", action="version", version=f"OpenConverter {__version__}")
    return parser


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    parser.parse_args(argv)
    return 0
