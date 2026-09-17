# The udev rule, and why it is not in a package yet

`60-esper-uinput.rules` grants the `uinput` group access to `/dev/uinput`,
which is what the `uinput` injection backend needs on a host where no display
server will inject for it.

**It is deliberately not installed by any package.** A package that drops a
rule into `/etc/udev/rules.d` is a package that changes what a group of users
on that machine can do, silently, as a side effect of installing a remote
desktop. That is a decision for whoever administers the host, and the install
is three commands they can read — printed by `esper-engine input` on any host
where the node exists and cannot be opened.

Packaging it as an opt-in file — installed but inert until a group exists — is
worth doing and is tracked. It needs an entry in `packaging/contents.tsv`, a
new kind in `scripts/check-packages.sh`'s kind-to-path mapping, and a line in
each of the three package definitions.
