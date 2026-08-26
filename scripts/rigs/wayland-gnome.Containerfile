# GNOME on Wayland, which is where most people are.
#
# # Why a second rig
#
# `wayland.Containerfile` is sway, and sway answered the questions it can:
# `zwlr_screencopy` for pixels, virtual keyboard and pointer for input, and a
# ScreenCast portal. What it cannot answer is anything about the **portal path
# for input**, because `xdg-desktop-portal-wlr` implements ScreenCast and
# nothing else — there is no RemoteDesktop portal on wlroots to measure.
#
# That matters because Fedora and Ubuntu are GNOME on Wayland by default, so
# the portal path is the one most people would meet, and it is the expensive
# one: it is the only reason this project would need a D-Bus dependency and a
# PipeWire dependency, neither of which the workspace has. A cost that large
# should be decided against a measurement rather than against documentation.
#
# So: mutter on its headless backend with a virtual monitor, and
# `xdg-desktop-portal-gnome` behind it. mutter is the compositor GNOME Shell is
# built on and it supports headless operation for exactly this purpose —
# GNOME's own continuous integration runs it this way.
#
# # What this rig is not
#
# It is not a GNOME desktop. There is no shell, no session manager and nobody
# to click "Share" in a consent dialog. That is a real limit: the portal's
# **presence** and its interfaces can be measured here, and a granted screencast
# cannot be, because granting is a person's act and this rig has no person. The
# checks say which of the two they are asking about.
#
# Build from the repository root:
#   docker build -f scripts/rigs/wayland-gnome.Containerfile -t esper-wayland-gnome .
FROM archlinux:base@sha256:522dd24e4a16f41afe71c7561febe519f7575404c291565e9f0be823d1794ca4

# One day's repositories, for the reason given in wayland.Containerfile: a
# digest pins the image's filesystem and, on a rolling distribution, nothing
# about the packages. Yesterday, because the archive publishes in arrears.
ARG ARCHIVE=2026/09/11
RUN printf 'Server=https://archive.archlinux.org/repos/%s/$repo/os/$arch\n' "$ARCHIVE" \
      > /etc/pacman.d/mirrorlist \
 && sed -i 's/^SigLevel.*/SigLevel = Never/' /etc/pacman.conf

# `libei` is here because it is the thing under question: the RemoteDesktop
# portal hands back an EIS socket, and libei is how a client speaks to it.
RUN pacman -Sy --noconfirm --needed \
      mutter \
      xdg-desktop-portal xdg-desktop-portal-gnome \
      pipewire pipewire-pulse wireplumber \
      dbus libei \
      wayland-utils \
 && pacman -Scc --noconfirm

# The same reasoning as the sway rig: a file capability and
# `--security-opt=no-new-privileges` cannot both hold, so the capability goes
# rather than the protection. mutter wants `cap_sys_nice` for the same
# scheduling reason sway does, and a rig needs neither.
RUN setcap -r /usr/bin/mutter 2>/dev/null || true

RUN useradd --create-home --home-dir /seat --uid 1000 rig \
 && mkdir -p /run/user/1000 \
 && chown rig:rig /run/user/1000 \
 && chmod 0700 /run/user/1000

COPY scripts/rigs/gnome-session.sh /usr/local/bin/gnome-session-rig
RUN chmod 0755 /usr/local/bin/gnome-session-rig

USER rig
ENV XDG_RUNTIME_DIR=/run/user/1000 \
    XDG_SESSION_TYPE=wayland \
    XDG_CURRENT_DESKTOP=GNOME \
    WAYLAND_DISPLAY=wayland-0
WORKDIR /seat

ENTRYPOINT ["/usr/local/bin/gnome-session-rig"]
