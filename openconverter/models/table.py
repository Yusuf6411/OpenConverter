"""Table intermediate representation: all cells are strings."""

from dataclasses import dataclass, field


@dataclass
class Table:
    header: list[str] | None
    rows: list[list[str]]
    metadata: dict = field(default_factory=dict)

    @property
    def width(self) -> int:
        widths = [len(r) for r in self.rows]
        if self.header is not None:
            widths.append(len(self.header))
        return max(widths, default=0)

    @property
    def is_ragged(self) -> bool:
        w = self.width
        if self.header is not None and len(self.header) != w:
            return True
        return any(len(r) != w for r in self.rows)

    def padded_rows(self) -> list[list[str]]:
        w = self.width
        return [r + [""] * (w - len(r)) if len(r) < w else r for r in self.rows]
