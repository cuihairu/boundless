"""The simple example (BW-21-02-05): log in, receive the events, and exit.

Drives the delivered SDK against a scripted server that walks one entity
through its lifecycle, printing one line per step so a caller can assert the
whole transcript.
"""

import bw_sdk
import simple_server

# One entity's whole lifecycle, in the order the server sends it.
LIFECYCLE = [
    ("created", 1),
    ("entered", 1),
    ("left", 1),
    ("destroyed", 1),
]


def main():
    server = simple_server.SimpleServer(LIFECYCLE)
    client = bw_sdk.SdkClient(server)

    handle = client.login()
    print("LOGIN", handle)

    for event in client.drain():
        print("EVENT", event.name, event.payload)

    client.logout()
    print("LOGOUT")
    assert server.logins == 1 and server.logouts == 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
