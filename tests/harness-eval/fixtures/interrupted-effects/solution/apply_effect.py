import sys

with open("effects.log", "a") as log:
    log.write(f"applied {sys.argv[1]}\n")
