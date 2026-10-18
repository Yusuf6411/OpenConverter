"""Format specification and option schemas shared by all converters."""

from dataclasses import dataclass, field
from typing import Callable

OWN = "OWN"
PARTIAL = "PARTIAL"
EXTERNAL = "EXTERNAL"


@dataclass(frozen=True)
class FormatSpec:
    id: str
    name: str
    category: str                      # image | text | data
    extensions: tuple[str, ...]
    mime: str
    sniff: Callable[[bytes], bool]
    can_read: bool
    can_write: bool
    ownership: str
    variants_note: str
    read_options: dict = field(default_factory=dict)
    write_options: dict = field(default_factory=dict)
    # Whether converting this format to itself can change the output.
    identity_meaningful: bool = False
    ownership_note: str = ""

    @property
    def primary_extension(self) -> str:
        return self.extensions[0]


def int_option(label, minimum, maximum, default, help_text=""):
    return {"type": "int", "label": label, "min": minimum, "max": maximum, "default": default, "help": help_text}


def bool_option(label, default, help_text=""):
    return {"type": "bool", "label": label, "default": default, "help": help_text}


def choice_option(label, choices, default, help_text="", labels=None):
    opt = {"type": "choice", "label": label, "choices": list(choices), "default": default, "help": help_text}
    if labels:
        opt["choice_labels"] = dict(labels)
    return opt


def colour_option(label, default, help_text=""):
    return {"type": "colour", "label": label, "default": default, "help": help_text}


@dataclass
class ConversionResult:
    data: bytes
    notes: list
    info: dict
