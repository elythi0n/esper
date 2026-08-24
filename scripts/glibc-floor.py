#!/usr/bin/env python3
"""The oldest glibc a binary can run on, read out of the binary.

# Why this exists

`scripts/build-deb.sh` wrote `Depends: libc6`, with no version, and that was
wrong in the way that is worst: it is *almost* right, so nothing complains.

    dpkg -i esper_0.0.0-1_amd64.deb     # Debian 12
    Setting up esper (0.0.0-1) ...
    $ esper-engine --version
    /usr/bin/esper-engine: /lib/x86_64-linux-gnu/libc.so.6:
        version `GLIBC_2.39' not found

The package installs. `dpkg -l` says `ii`. The file is there, it is executable,
it is the right size, and it cannot start. `libc6` was satisfied, because every
Debian has a `libc6` — the dependency was true and meant nothing.

Debian's own tooling solves this with `dpkg-shlibdeps`, which is part of the
source-package machinery this project deliberately does not use: the whole
point of assembling the archive from `ar` and `tar` is that the package can be
built, and therefore checked, on a machine that is not Debian. So the version
has to come from somewhere else, and the binary already knows it.

# What it reads

A dynamically linked ELF records, in `.gnu.version_r`, every symbol version it
needs from every library it needs — `GLIBC_2.39` appears there because the
linker resolved a symbol that first existed in glibc 2.39. The highest such
version is the oldest glibc that can run the file, and it is a fact about the
bytes rather than about the machine that produced them.

That is a different question from the one `ldd` answers, which is whether *this*
machine can run it, and from the one a build script guesses, which is whether
the author remembered.

# Usage

    scripts/glibc-floor.py <binary> [<binary>...]

Prints one version — the highest floor across every file given — or nothing at
all if none of them links glibc dynamically. Exit 2 if a file is not an ELF.
"""

import struct
import sys
from pathlib import Path

# The only ELF the build produces: 64-bit, little-endian. A 32-bit or
# big-endian target would need the offsets below to be a table rather than a
# set of constants, and refusing is better than reading them wrongly.
ELF64_LE = (2, 1)


class NotAnELF(Exception):
    pass


def sections(data: bytes) -> dict[str, tuple[int, int, int]]:
    """Every section, by name → (offset, size, link)."""
    if data[:4] != b"\x7fELF":
        raise NotAnELF("not an ELF file")
    if (data[4], data[5]) != ELF64_LE:
        raise NotAnELF("not a 64-bit little-endian ELF")

    e_shoff, = struct.unpack_from("<Q", data, 0x28)
    e_shentsize, e_shnum, e_shstrndx = struct.unpack_from("<HHH", data, 0x3A)
    if e_shoff == 0 or e_shnum == 0:
        return {}

    def header(i: int) -> tuple[int, int, int, int]:
        base = e_shoff + i * e_shentsize
        sh_name, = struct.unpack_from("<I", data, base)
        sh_offset, = struct.unpack_from("<Q", data, base + 0x18)
        sh_size, = struct.unpack_from("<Q", data, base + 0x20)
        sh_link, = struct.unpack_from("<I", data, base + 0x28)
        return sh_name, sh_offset, sh_size, sh_link

    _, names_off, _, _ = header(e_shstrndx)

    def name_at(offset: int) -> str:
        end = data.index(b"\0", names_off + offset)
        return data[names_off + offset:end].decode("utf-8", "replace")

    found = {}
    for i in range(e_shnum):
        sh_name, sh_offset, sh_size, sh_link = header(i)
        found[name_at(sh_name)] = (sh_offset, sh_size, sh_link)
    return found


def string_at(data: bytes, table: int, offset: int) -> str:
    end = data.index(b"\0", table + offset)
    return data[table + offset:end].decode("utf-8", "replace")


def needed_versions(path: Path) -> set[tuple[int, ...]]:
    """Every glibc symbol version the file needs."""
    data = path.read_bytes()
    found = sections(data)
    if ".gnu.version_r" not in found:
        # Statically linked, or linked against no versioned library. Go
        # binaries built without cgo land here, and they have no floor.
        return set()

    offset, size, link = found[".gnu.version_r"]
    # sh_link names the string table these entries index into, by section
    # number. Reading it from the section rather than assuming `.dynstr` is
    # what keeps this correct on a file whose linker ordered things otherwise.
    e_shoff, = struct.unpack_from("<Q", data, 0x28)
    e_shentsize, _, _ = struct.unpack_from("<HHH", data, 0x3A)
    strtab_off, = struct.unpack_from("<Q", data, e_shoff + link * e_shentsize + 0x18)

    versions: set[tuple[int, ...]] = set()
    at = offset
    end = offset + size
    while at < end:
        vn_cnt, = struct.unpack_from("<H", data, at + 2)
        vn_file, vn_aux, vn_next = struct.unpack_from("<III", data, at + 4)
        library = string_at(data, strtab_off, vn_file)

        aux = at + vn_aux
        for _ in range(vn_cnt):
            vna_name, vna_next = struct.unpack_from("<II", data, aux + 8)
            tag = string_at(data, strtab_off, vna_name)
            if library.startswith("libc.so") and tag.startswith("GLIBC_"):
                number = tag[len("GLIBC_"):]
                try:
                    versions.add(tuple(int(p) for p in number.split(".")))
                except ValueError:
                    pass
            if vna_next == 0:
                break
            aux += vna_next

        if vn_next == 0:
            break
        at += vn_next
    return versions


def main(argv: list[str]) -> int:
    if not argv:
        print(__doc__.strip().split("\n\n")[0], file=sys.stderr)
        return 2

    highest: tuple[int, ...] | None = None
    for name in argv:
        path = Path(name)
        try:
            for version in needed_versions(path):
                if highest is None or version > highest:
                    highest = version
        except NotAnELF as why:
            print(f"{path}: {why}", file=sys.stderr)
            return 2
        except (OSError, struct.error, ValueError) as why:
            print(f"{path}: cannot be read as an ELF: {why}", file=sys.stderr)
            return 2

    if highest is not None:
        print(".".join(str(p) for p in highest))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
