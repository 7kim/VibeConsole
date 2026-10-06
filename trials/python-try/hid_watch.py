import os
import time

DEVICE = "/dev/hidraw0"

fd = os.open(
    DEVICE,
    os.O_RDONLY | os.O_NONBLOCK
)

print(f"Watching {DEVICE}")
print()
print("Do this:")
print("1. Press Pad 1")
print("2. Hold Pad 1")
print("3. Press Octave -")
print("4. Press Octave +")
print("5. Ctrl+C when finished")
print()

try:
    while True:
        try:
            data = os.read(fd, 32)

            if data:
                print(
                    f"{len(data):2d} bytes:",
                    " ".join(f"{b:02X}" for b in data)
                )

        except BlockingIOError:
            pass

        time.sleep(0.005)

except KeyboardInterrupt:
    print("\nStopped.")

finally:
    os.close(fd)
