# A Fedora with exactly what the spec asks for, and nothing chosen twice.
#
# # Why an image at all
#
# `packaging/rpm/esper.spec` has been written since Phase 16 opened and had
# never been built: this project is developed on Arch, which has no `rpmbuild`.
# The spec said so at the top of the file, which was honest and is not the same
# as checked — a `%files` list can name a path the build does not produce, and
# the only thing that finds that is `rpmbuild` refusing.
#
# The tooling gets its own image rather than being installed per run because
# `dnf install` of a Go and a Rust toolchain is minutes, and a check nobody
# wants to wait for is a check that gets run once.
#
# # Why `builddep` rather than a list
#
# The spec already names what it needs to build — `BuildRequires` is exactly
# that list. Writing those package names again here would be a second list to
# forget to update, and the failure it causes is confusing: the spec asks for
# `nasm`, the image does not have it, and the error arrives from inside a
# compile. `dnf builddep` reads the spec, so the spec stays the one place.
#
# Build from the repository root:
#   docker build -f packaging/container/rpmbuild.Containerfile -t esper-rpmbuild .
#
# The base is pinned by digest — see packaging/images.tsv for why.
FROM fedora:43@sha256:1ad64e0ebefee83263fffacc5cb139b37ff70def215e4fe44720896c83353132

RUN dnf install -y rpm-build rpmdevtools dnf-command\(builddep\) \
 && dnf clean all

COPY packaging/rpm/esper.spec /tmp/esper.spec
RUN dnf builddep -y /tmp/esper.spec && dnf clean all

# rpmbuild refuses to run as root for good reasons, and the build it is running
# is this project's own `make`, which the repository already guards against
# root. A plain user with a home directory is what both want.
#
# That home is `/build` rather than the usual place under the home directory
# tree, because `make check-isolation` refuses any absolute path there: it
# cannot tell a container's filesystem from the author's laptop, and a rule
# that has to tell them apart is a rule with a hole in it. Nothing here needs
# the home to be anywhere in particular.
RUN useradd --create-home --home-dir /build --uid 1000 builder
USER builder
WORKDIR /build
RUN rpmdev-setuptree
