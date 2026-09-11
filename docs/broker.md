# Running a broker

A broker introduces two machines that cannot dial each other. It is the one
piece of Esper that anybody runs on a server, and it is optional: most people
never need one.

This takes about five minutes. What has and has not been run is written down
at the [end](#how-this-was-checked).

## Do you need one?

Probably not. Esper connects two devices directly whenever it can — on the same
network, or through routers that allow it, which most home routers do. A broker
is for the case where neither end can be reached from outside and direct
connection fails.

Try it without one first. If `esper connect` finds the other machine, you are
finished.

## What it can and cannot see

A broker is told two fingerprints and puts two peers in touch. It is **not** in
the session:

- it holds no session keys and decrypts nothing;
- it stores nothing — no peer list, no session record, no log of who met whom;
- it introduces two peers only when **each has named the other**, so a stranger
  who learns your fingerprint can connect to the broker, ask, and wait for ever;
- every request is signed by the device key it claims to come from.

What it does see is that two fingerprints were introduced at a moment in time,
and the addresses they were introduced at. It does not write that down.

Turning on `--relay` changes one thing: traffic then flows through the machine
instead of past it. It is still encrypted end to end, and the broker still
cannot read it, but it now costs your bandwidth for as long as a session lasts.
It is off by default for that reason.

## Choose how to run it

### A container

The image has no shell, no package manager and no root, and the filesystem is
read-only apart from the one directory holding the key.

```sh
docker build -f packaging/container/Containerfile -t esper-broker .
docker volume create esper-broker
docker run -d --name esper-broker \
  --read-only --cap-drop=ALL --security-opt=no-new-privileges \
  -v esper-broker:/var/lib/esper-broker \
  -p 7443:7443/udp esper-broker
```

The key is made on first start and kept in the volume. `docker logs
esper-broker` prints it.

### A package

```sh
sudo dpkg -i esper-broker_*.deb        # Debian, Ubuntu
sudo dnf install ./esper-broker-*.rpm  # Fedora, RHEL
sudo systemctl enable --now esper-broker
```

The unit runs the broker under a `DynamicUser` with a state directory only it
can read, and the key is made on first start.

### From source

```sh
esper-broker serve --listen :7443
```

`--identity <path>` puts the key somewhere other than beside this machine's
Esper state.

## Get the key, and hand it out

Peers pin the key. Print it with:

```sh
esper-broker identity
```

```
broker identity

    linen elder squad velvet
    kettle fedora jubilee total

  key   c93ec016ee72f99f7661169d9f1ba855b2fc6bd79ed09c047f362ece6ef27406
  file  /home/somebody/.esper/broker.key
```

For a script rather than a person, `esper-broker identity --json` answers with
`public_key`, `fingerprint` and `file` — the same three field names `esper
identity --json` uses for the same facts. `public_key` is what goes after
`--broker-key`.

Under systemd the key belongs to the service's own state directory, so read it
from the log the service wrote when it started:

```sh
sudo journalctl -u esper-broker | head -20
```

In a container, `docker logs esper-broker`.

Give people the address and the key together:

```sh
esper-engine invite --via broker.example.com:7443 --broker-key c93ec016…7406
```

**The whole key, not the fingerprint.** Sixteen hex characters is for reading
aloud; pinning against sixty-four bits would be a security decision made on a
birthday bound. The engine refuses a fingerprint here by its length.

The eight words are the same key in a form two people can check over the phone.

## Open the port

UDP, not TCP.

```sh
sudo ufw allow 7443/udp                                  # Debian, Ubuntu
sudo firewall-cmd --permanent --add-port=7443/udp        # Fedora, RHEL
sudo firewall-cmd --reload
```

Port 7443 is above 1024, so the broker needs no privilege to bind it. Put a
firewall redirect in front if you want it on 443.

## Check that it works

From another machine, with the address and key you just handed out:

```sh
esper-engine invite --via broker.example.com:7443 --broker-key <the key>
```

An invite link comes back if the broker answered. If it does not, the broker
was not reached: check the UDP port, and check the key matches — a wrong key is
refused rather than ignored, which is the point of pinning it.

## If you lose the key

Every peer that pinned it has to be given the new one. That is not a flaw in
the design, it is the design: a broker whose key can change silently is a
broker that can be replaced silently.

Keep the volume, or the state directory, or take a copy of the key file.

## Updating

The broker speaks a protocol version, and two peers and a broker have to agree
on it. `esper-broker version` prints what this one accepts, and
[`compatibility.md`](compatibility.md) says which versions work together.

## How this was checked

Because a guide that has not been followed is a guess about a guide.

**Run, end to end, on this machine.** The container was built from
`packaging/container/Containerfile`, started with exactly the `docker run` above,
and printed its key. It was restarted and printed the *same* key, which is the
volume doing its job. A real `esper-engine invite --via … --broker-key …` was
then sent to it from outside the container and came back with an invite link —
so the published UDP port, the pinning and the introduction all work rather than
merely compile.

**Run in a container of the distribution it names.** `dpkg -i` on Debian 13 and
`dnf install` on Fedora 43, both from a package this project built, followed by
running the binary. `make check-install` and `make check-rpm` do this on demand.

**Checked without being run.** `systemctl enable --now esper-broker` is not
executed here: doing it would need a container running systemd as PID 1, which
means giving that container privileges this project is not willing to hand out
to a test. What *is* checked is the part that was actually broken — the unit
installed by the package was starting `/usr/local/bin/esper-broker`, which no
package installs — and `make check-rpm` now has systemd itself read the
installed unit and accept it.

**Not run at all.** The `ufw` and `firewall-cmd` lines, and `journalctl`. They
are each distribution's ordinary command for the job and this machine uses
none of the three.
