# Packaging

Two things get installed, and they are not installed the same way.

## The formats, and what is checked about them

| | Where | Built here? |
|---|---|---|
| Arch | [`arch/PKGBUILD`](arch/PKGBUILD) | by `makepkg`, not by the project |
| Debian, Ubuntu | `make package-deb` → `dist/*.deb` | **yes**, by [`../scripts/build-deb.sh`](../scripts/build-deb.sh) |
| Fedora, RHEL | [`rpm/esper.spec`](rpm/esper.spec) | **yes**, by `make check-rpm`, in a Fedora container |

All three install from one list, [`contents.tsv`](contents.tsv), through one
script. Three package definitions describing the same software in three
dialects is three chances to forget a file, and a package missing
`esper-engine` installs cleanly and does nothing.

`make check-packages` asks the question that survives that. The packages agree
with the manifest by construction, so agreeing with it proves nothing — what it
checks is that **everything the build produced is in some package**, which is
answered from `build/` and not from the manifest.

The RPM spec's `%files` list is the one place a format forces the manifest to
be written twice, so that list is compared against the manifest rather than
trusted.

For most of this project's life the spec had never been built — Arch has no
`rpmbuild` — and that was stated at the top of the file rather than hidden.
`make check-rpm` now builds it in a pinned Fedora container and installs the
result on a clean one. It found three defects on the first run, none of them
visible by reading: see the comment at the top of the spec.

## Installing is a different question from packaging

`make check-packages` asks whether everything the build produced is in some
package. It answers that here, on Arch, from `build/`.

`make check-install` asks whether `dpkg -i` works, on Debian, which this
machine is not — and the first time anything asked, the answer was no. The
package installed on Debian 12 and `esper-engine` could not start, because
`Depends: libc6` is satisfied by every Debian ever released and the binary
needs `GLIBC_2.39`. The floor is now read out of the binaries themselves
([`../scripts/glibc-floor.py`](../scripts/glibc-floor.py)), and the check
offers the package to a Debian too old for it to prove the floor bites.

It found a second one of the same shape: both packages installed a systemd
unit starting a binary the package does not install — `%h/.local/bin/esper` and
`/usr/local/bin/esper-broker`, which are the paths a *hand* install uses. Units
are now rewritten as they are staged, and every installed unit is held to
starting a file its own package installed.

| | Who runs it | How |
|---|---|---|
| **The engine** on your own machine | you | a **user** service — [`systemd/esper.service`](systemd/esper.service) |
| **A broker**, if you run one | an operator, on a server | a **system** service — [`systemd/esper-broker.service`](systemd/esper-broker.service) — or a [container](container/Containerfile) |

## The engine is a user service, deliberately

Esper runs as the person whose screen and shell it exposes. It needs no
privilege to capture a display that user is already looking at, or to start a
shell as them, and running it as root would turn a compromise of the engine
into a compromise of the machine rather than of one account.

See [`systemd/README.md`](systemd/README.md) for installing it, for the
lingering a headless server needs, and for what the hardening in the unit does
and deliberately does not do.

## The broker is the part that faces the internet

It introduces two peers to each other and, if you ask it to, forwards bytes
between them. It holds no session keys, decrypts nothing, and keeps nothing on
disk but its own identity — and it is still the piece worth hardening most,
because a component with little to steal is still a component with a listening
socket that strangers can reach.

Both packagings say the same thing in their own dialect:

- **A user of its own**, with no shell and no home. `DynamicUser=yes` in the
  unit, `nonroot` in the container.
- **No capabilities, and no way to gain any.** `CapabilityBoundingSet=` and
  `NoNewPrivileges=` in the unit; `--cap-drop=ALL --security-opt=no-new-privileges`
  at run time for the container.
- **A filesystem it cannot write to**, apart from the one directory holding its
  key: `StateDirectory=` in the unit, `--read-only` plus a volume for the
  container.
- **One socket, one family.** UDP over IPv4 and IPv6 and nothing else — no Unix
  sockets, no netlink, no raw sockets.
- **A syscall filter** limited to `@system-service`, minus the privileged,
  resource and obsolete groups.

`systemd-analyze security esper-broker.service` scores it. At the time of
writing it reports an exposure of **1.1**, and everything left is inherent to
being a network service: it needs internet sockets, it needs the host's
network, and it cannot have an IP allow list because anybody may be introduced
through it.

### Running one

```sh
sudo cp esper-broker /usr/local/bin/
sudo cp systemd/esper-broker.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now esper-broker
sudo systemctl status esper-broker      # prints the key
```

Open **UDP 7443** to it, and give people the address and the key:

```sh
esper-engine connect elythion --via broker.example.com:7443 --broker-key <the key>
```

The key is printed on first start and is not a secret. Pinning it is what
stops somebody else's broker being used instead of yours — which is why the
address alone is never enough.

### Forwarding is off unless you ask

Introducing two peers costs a few packets. Forwarding for them costs **your**
bandwidth for as long as their session lasts, so nobody should discover they
are paying for that because a default said so:

```sh
sudo systemctl edit esper-broker
# [Service]
# ExecStart=
# ExecStart=/usr/local/bin/esper-broker serve --listen 0.0.0.0:7443 \
#     --identity ${STATE_DIRECTORY}/broker.key --relay
```

Even then the broker sees ciphertext: relaying moves bytes between two peers
that have already agreed keys with each other, and it is not one of them.

### In a container

```sh
docker build -f packaging/container/Containerfile -t esper-broker .
docker volume create esper-broker
docker run -d --name esper-broker \
  --read-only --cap-drop=ALL --security-opt=no-new-privileges \
  -v esper-broker:/var/lib/esper-broker \
  -p 7443:7443/udp esper-broker
docker logs esper-broker
```

The image is distroless: a C runtime and nothing else. No shell means there is
no shell to get.

Keep the volume. The key lives there, and losing it means every user who
pinned it has to be given a new one.

## Containers

Two of them, and they are not for the same thing.

[`container/Containerfile`](container/Containerfile) is a way to **run** the
broker: distroless, no shell, no root, read-only except the directory holding
the key. [`container/rpmbuild.Containerfile`](container/rpmbuild.Containerfile)
is a way to **check** a package, by being a Fedora.

Every image either of them uses, and every image any check starts, is written
down once in [`images.tsv`](images.tsv) with a digest, a date and a reason. A
tag is a pointer this project does not hold — `debian:13-slim` is a different
filesystem this month than last — and pulling by one is both a supply-chain
decision made by nobody and a hole in a project that checks it builds the same
bytes twice.

`make check-images` holds the rule: no `FROM` without a digest, no digest that
is not in the manifest, and no container started outside
[`../scripts/in-container.sh`](../scripts/in-container.sh), which is what takes
the network away and drops the capabilities. `scripts/update-image.sh` moves a
pin forward and dates it; `--check` reports drift without changing anything.
