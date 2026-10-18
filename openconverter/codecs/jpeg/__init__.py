"""Baseline JPEG codec (our own implementation)."""

from openconverter.codecs.jpeg.decoder import decode, sniff
from openconverter.codecs.jpeg.encoder import encode

__all__ = ["decode", "encode", "sniff"]
