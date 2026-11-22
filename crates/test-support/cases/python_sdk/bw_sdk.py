"""The boundless SDK for Python (BW-21-02-05).

A client is bound to one server object: it asks the server to log it in,
pulls the events the server has for it in order, and logs out.  Every call
into the server goes through the same three methods, so a test can stand in
for a live server with a scripted object.

Receiving or logging out before logging in is a refusal, not a no-op: the
SDK raises, so a caller tells a mis-ordered client from a logged-in one.
"""


class Event:
    """One event the server sent: its name and its payload."""

    __slots__ = ("name", "payload")

    def __init__(self, name, payload=None):
        self.name = name
        self.payload = payload

    def __repr__(self):
        return "Event(%r, %r)" % (self.name, self.payload)


class SdkClient:
    """One SDK client, bound to one server."""

    def __init__(self, server):
        self.server = server
        self.logged_in = False

    def login(self):
        """Log in; the server records the login and returns the handle."""
        handle = self.server.login(self)
        if handle is None:
            raise RuntimeError("login refused")
        self.logged_in = True
        return handle

    def recv(self):
        """Receive the next event, or None when the server has none left."""
        self._require_login()
        got = self.server.next_event(self)
        if got is None:
            return None
        name, payload = got
        return Event(name, payload)

    def drain(self):
        """Receive every pending event, in order, stopping at the last one."""
        events = []
        while True:
            event = self.recv()
            if event is None:
                return events
            events.append(event)

    def logout(self):
        """Log out; the server records the logout and drops the client."""
        self._require_login()
        self.server.logout(self)
        self.logged_in = False

    def _require_login(self):
        if not self.logged_in:
            raise RuntimeError("not logged in")
