"""The shared case contract; fixtures drive the executable through a Context."""

from dataclasses import dataclass
from typing import Callable


@dataclass(frozen=True)
class Case:
    """One independently isolated, selectable executable-boundary scenario."""

    id: str
    description: str
    run: Callable
    comparison: str = "N"
    slow: bool = False
    pending: str | None = None
    platforms: tuple[str, ...] = ("darwin", "linux")
    # Initial schedule hint only; shared fixtures detect and promote actual workloads.
    exclusive: bool = False

    @property
    def area(self):
        """Return the matrix area identified by the stable case prefix."""
        return {"W": "wire", "C": "cli", "T": "tools", "A": "agents"}.get(self.id[:1], "unknown")
