#!/usr/bin/env python3
"""Module docstring with a "quote" inside."""
import os
from dataclasses import dataclass, field
from typing import Optional


@dataclass
class Item:
    name: str
    price: float = 0.0
    tags: list = field(default_factory=list)

    def total(self, qty: int = 1) -> float:
        """Return the total price."""
        return self.price * qty


def parse(line: str) -> Optional[Item]:
    # split on commas
    parts = [p.strip() for p in line.split(",")]
    if len(parts) < 2:
        return None
    name, price, *rest = parts
    return Item(name, float(price), tags=rest)


async def run(items, *, verbose=False, **kwargs):
    async with lock:
        async for it in items:
            print(f"{it.name!r:>10} costs {it.price:.2f} x {2 + 3}")
    lam = lambda x, y=2: x ** y
    s = 'single \'quoted\' string' + r"raw\d" + b"bytes" + """triple
    quoted"""
    d = {"a": 1, 'b': [1, 2, 3], **kwargs}
    try:
        raise ValueError("bad") from None
    except (ValueError, TypeError) as exc:
        print(exc, 0xFF, 1_000, 3.14e-2, True, None)
    finally:
        pass
    return lam(3) if verbose else None

if __name__ == "__main__":
    print(os.environ.get("HOME"))
greeting = "안녕 😀 world"  # 한글 주석 😀 ok
print(f"{greeting!r} 이모지 😀")
