# A Wayland desktop, so that Wayland claims can be checked by something.
#
# # Why this exists
#
# Every Wayland item in the tracker is deferred, and all of them for the same
# reason: the machine this project is developed on runs X11 under XFCE, with no
# ScreenCast portal and no RemoteDesktop portal installed. So "capture works on
# Wayland" and "input works on Wayland" have never been statements anybody here
# could make true or false — and on a default Fedora or Ubuntu desktop today,
# which is GNOME on Wayland, Esper can do neither.
#
# Installing a compositor on the development machine is the obvious move and
# the wrong one: it would put a second display stack, a portal service and a
# PipeWire daemon on somebody's working desktop to answer a question about
# somebody else's. A container is the same trick used for the packaging work —
# be the other machine, rather than reshape this one.
#
# # What this rig is, and what it is not
#
# It is **sway**, a wlroots compositor, on the headless backend, with
# `xdg-desktop-portal-wlr` behind it. That combination gives a real Wayland
# session and a real **ScreenCast** portal over D-Bus.
#
# It does **not** give a RemoteDesktop portal, and that is a property of
# wlroots rather than of this image: `xdg-desktop-portal-wlr` implements
# ScreenCast and nothing else. Input injection through a portal is GNOME's and
# KDE's to provide, so it needs its own rig — see `wayland-gnome.Containerfile`.
# Two rigs is the honest shape of the problem, not a workaround.
#
# # Why the packages are pinned to a day
#
# Arch is a rolling distribution, so pinning the base image by digest fixes the
# filesystem and not the repository: `pacman -Syu` on two different days
# installs two different compositors. The Arch Linux Archive keeps a dated
# snapshot of every repository, so the mirrorlist points at one day. The image
# is then the same image tomorrow, which is what a pin is for.
#
# Build from the repository root:
#   docker build -f scripts/rigs/wayland.Containerfile -t esper-wayland .
FROM archlinux:base@sha256:522dd24e4a16f41afe71c7561febe519f7575404c291565e9f0be823d1794ca4

# One day's repositories, not "whatever is current". See above.
#
# Yesterday, not today: the archive publishes a day in arrears, and asking for
# today's snapshot is a 404 that reads as a broken mirror.
ARG ARCHIVE=2026/09/11
RUN printf 'Server=https://archive.archlinux.org/repos/%s/$repo/os/$arch\n' "$ARCHIVE" \
      > /etc/pacman.d/mirrorlist \
 && sed -i 's/^SigLevel.*/SigLevel = Never/' /etc/pacman.conf

# sway brings the compositor; xdg-desktop-portal and its wlroots backend bring
# the ScreenCast portal; pipewire carries the frames the portal hands out.
# `grim` is not Esper's — it is a second opinion, so that "the portal produced a
# frame" can be checked by something that is not the code under test.
RUN pacman -Sy --noconfirm --needed \
      sway \
      xdg-desktop-portal xdg-desktop-portal-wlr \
      pipewire pipewire-pulse wireplumber \
      dbus libei \
      grim wayland-utils \
 && pacman -Scc --noconfirm

# Arch ships sway with `cap_sys_nice=ep`, which it wants for realtime
# scheduling. A file capability and `--security-opt=no-new-privileges` cannot
# both hold: the kernel refuses to exec the file at all, and the error is
# `Operation not permitted` with nothing to say it was about a capability.
#
# The capability is removed rather than the protection: a rig has no use for
# realtime priority, and dropping `no-new-privileges` to run a compositor would
# weaken every container this image is ever used for to fix a scheduling hint.
RUN setcap -r /usr/bin/sway

# A plain user with a runtime directory, because a Wayland session is a thing a
# person has and `XDG_RUNTIME_DIR` must be theirs and 0700. Root would work and
# would be a different machine than the one this is standing in for.
RUN useradd --create-home --home-dir /seat --uid 1000 rig \
 && mkdir -p /run/user/1000 \
 && chown rig:rig /run/user/1000 \
 && chmod 0700 /run/user/1000

COPY scripts/rigs/wayland-session.sh /usr/local/bin/wayland-session
RUN chmod 0755 /usr/local/bin/wayland-session

# `WLR_HEADLESS_OUTPUTS` because the headless backend creates no output unless
# asked, and a compositor with no output accepts a capture and returns nothing —
# a success that produces no pixels. `WLR_RENDERER=pixman` because there is no
# DRM device in a container: without it wlroots fails at `drmGetDevices2` and
# the session never starts.
USER rig
ENV XDG_RUNTIME_DIR=/run/user/1000 \
    XDG_SESSION_TYPE=wayland \
    XDG_CURRENT_DESKTOP=sway \
    WLR_BACKENDS=headless \
    WLR_LIBINPUT_NO_DEVICES=1 \
    WLR_HEADLESS_OUTPUTS=1 \
    WLR_RENDERER=pixman \
    WAYLAND_DISPLAY=wayland-1
WORKDIR /seat

# Nothing starts by itself: `wayland-session <command>` brings the session up,
# runs the command inside it, and takes it down. A rig that leaves daemons
# running after the thing it was measuring has finished is a rig that measures
# the previous run.
ENTRYPOINT ["/usr/local/bin/wayland-session"]
