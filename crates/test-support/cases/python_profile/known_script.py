# The known script fixture for BW-23-01-02: one heavy function and one cheap
# function, so the capture can prove it locates the function the time went to.

def burn(count):
    total = 0
    for _ in range(count):
        # Keep the value 32-bit: the loop is CPU work, not an unbounded
        # big-integer multiply (which grows quadratically in digit count).
        total = (total * 3 + 1) & 0xFFFFFFFF
    return total


def cheap():
    return 7


burn(200000)
cheap()
