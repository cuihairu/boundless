"""The simple example against a server that refuses the login (BW-21-02-05).

The SDK raises; the example reports the refusal and exits non-zero, so a
caller tells a refused login from a clean run by the exit status.
"""

import bw_sdk


class RefusingServer:
    """A server that will not log the client in."""

    def login(self, client):
        return None

    def next_event(self, client):
        return None

    def logout(self, client):
        raise AssertionError("never logged in")


def main():
    client = bw_sdk.SdkClient(RefusingServer())
    try:
        client.login()
    except RuntimeError as error:
        print("LOGIN-REFUSED", error)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
