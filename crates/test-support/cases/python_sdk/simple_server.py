"""A scripted server for the simple example (BW-21-02-05).

The SDK talks to a server through three methods: log in, take the next
event, and log out.  This server logs every client in with the same handle
and feeds it a fixed list of events in order, so the example's transcript is
reproducible.
"""


class SimpleServer:
    """A server that logs a client in, then feeds it a fixed event list."""

    def __init__(self, events=()):
        self.events = list(events)
        self.handle = 1
        self.logins = 0
        self.logouts = 0

    def login(self, client):
        self.logins += 1
        return self.handle

    def next_event(self, client):
        if not self.events:
            return None
        return self.events.pop(0)

    def logout(self, client):
        self.logouts += 1
