#!/usr/bin/env python3
"""
A RemoteDesktop portal that answers, so the client can be checked without one.

# Why a stub and not the real thing

The real portal is checked too — `check-wayland` runs this client against
`xdg-desktop-portal-gnome` in a container — but it can only ever establish
*where* the conversation stops, because a container has nobody in it to answer
a consent dialog. Everything past that dialog is unreachable: the grant, the
token it hands back, and the token being sent again next time. That is exactly
the part unattended access rests on.

So this answers: `CreateSession`, `SelectDevices` and `Start`, the way the
portal documents them, and records what it was asked. It is not a compositor
and injects nothing.

Usage: stub-portal.py <record-file>
"""
import json
import os
import sys

import dbus
import dbus.service
import dbus.mainloop.glib
from gi.repository import GLib

PORTAL = "org.freedesktop.portal.Desktop"
PATH = "/org/freedesktop/portal/desktop"
REMOTE_DESKTOP = "org.freedesktop.portal.RemoteDesktop"
REQUEST = "org.freedesktop.portal.Request"

# What the portal hands back for a grant it agreed to keep. A fixed string so
# the check can assert the client sent back *this* one rather than something
# shaped like it.
TOKEN = "a-grant-somebody-gave-once"


class Request(dbus.service.Object):
    """One request object, which answers with a Response signal."""

    def __init__(self, bus, path):
        super().__init__(bus, path)

    @dbus.service.signal(REQUEST, signature="ua{sv}")
    def Response(self, code, results):
        pass

    @dbus.service.method(REQUEST, in_signature="", out_signature="")
    def Close(self):
        pass


class Portal(dbus.service.Object):
    def __init__(self, bus, record):
        super().__init__(bus, PATH)
        self.bus = bus
        self.record = record
        self.asked = []
        self.sessions = 0

    def _answer(self, options, results):
        """Reply the way the portal does: a handle now, a signal in a moment."""
        token = str(options.get("handle_token", "t"))
        # The path the client is listening on. The portal derives it from the
        # caller's unique name with dots and dashes turned into underscores.
        path = self._request_path(token)
        request = Request(self.bus, path)
        GLib.idle_add(lambda: (request.Response(0, results), False)[1])
        return dbus.ObjectPath(path)

    def _request_path(self, token):
        sender = self.last_sender.replace(":", "").replace(".", "_")
        return f"/org/freedesktop/portal/desktop/request/{sender}/{token}"

    def _note(self, member, options):
        self.asked.append(
            {
                "member": member,
                "options": {k: _plain(v) for k, v in options.items()},
            }
        )
        with open(self.record, "w", encoding="utf-8") as f:
            json.dump(self.asked, f, indent=1)

    @dbus.service.method(
        REMOTE_DESKTOP, in_signature="a{sv}", out_signature="o", sender_keyword="sender"
    )
    def CreateSession(self, options, sender=None):
        self.last_sender = sender
        self._note("CreateSession", options)
        self.sessions += 1
        handle = f"/org/freedesktop/portal/desktop/session/stub/{self.sessions}"
        return self._answer(options, {"session_handle": dbus.String(handle)})

    @dbus.service.method(
        REMOTE_DESKTOP, in_signature="oa{sv}", out_signature="o", sender_keyword="sender"
    )
    def SelectDevices(self, session, options, sender=None):
        self.last_sender = sender
        self._note("SelectDevices", options)
        return self._answer(options, {})

    @dbus.service.method(
        REMOTE_DESKTOP,
        in_signature="osa{sv}",
        out_signature="o",
        sender_keyword="sender",
    )
    def Start(self, session, parent, options, sender=None):
        self.last_sender = sender
        self._note("Start", options)
        results = {
            # Keyboard and pointer, which is what the client insists on.
            "devices": dbus.UInt32(3),
        }
        # A grant is kept only when the client asked for it to be. A portal
        # that handed a token to a client which never asked would let this
        # check pass on a client that does not persist anything.
        # `ESPER_STUB_NO_PERSIST` is a portal that will not keep a grant,
        # which several do not. What the client says about *that* is what
        # decides whether a machine may advertise unattended access.
        keeps = not os.environ.get("ESPER_STUB_NO_PERSIST")
        if keeps and any(
            a["member"] == "SelectDevices" and a["options"].get("persist_mode")
            for a in self.asked
        ):
            results["restore_token"] = dbus.String(TOKEN)
        return self._answer(options, results)


def _plain(value):
    """A dbus value as something json can write."""
    if isinstance(value, dbus.Boolean):
        return bool(value)
    if isinstance(value, (dbus.UInt32, dbus.Int32, dbus.UInt16, dbus.Int16)):
        return int(value)
    if isinstance(value, dbus.Array):
        return [_plain(v) for v in value]
    return str(value)


def main():
    dbus.mainloop.glib.DBusGMainLoop(set_as_default=True)
    bus = dbus.SessionBus()
    name = dbus.service.BusName(PORTAL, bus)
    portal = Portal(bus, sys.argv[1])
    portal.last_sender = ":0.0"
    _ = name
    print("stub portal ready", flush=True)
    GLib.MainLoop().run()


if __name__ == "__main__":
    main()
