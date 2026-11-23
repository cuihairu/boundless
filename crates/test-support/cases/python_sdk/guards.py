"""The SDK's pre-login guards (BW-21-02-05).

A client that has not logged in may not receive events or log out: both
refuse with the same message, so a mis-ordered client is diagnosable.
"""

import bw_sdk


class SilentServer:
    """A server that would serve events, if the client were logged in."""

    def login(self, client):
        return 1

    def next_event(self, client):
        return ("created", 1)

    def logout(self, client):
        return None


def main():
    client = bw_sdk.SdkClient(SilentServer())
    for step, action in (("recv", client.recv), ("logout", client.logout)):
        try:
            action()
        except RuntimeError as error:
            print("GUARD", step, ":", error)
        else:
            print("GUARD", step, ": accepted before login")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
