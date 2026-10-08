"""Drives the scripted controller of a Xenia run started with `run.ps1 -Drive`.

    py tools/xenia/drive.py INPUT STEP...      # INPUT = the run's .input file
    py tools/xenia/drive.py INPUT recipe.txt   # a file of steps, one per line

Steps (button names: A B X Y LB RB START BACK LS RS UP DOWN LEFT RIGHT GUIDE):
    press BTN [BTN...] [ms]   hold for ms (default 120), release, settle 250 ms
    hold BTN [BTN...] SEC     hold for SEC seconds, then release
    stick L|R X Y [SEC]       set a thumbstick (-1..1); with SEC, recentre after
    trigger L|R V [SEC]       set a trigger (0..1); with SEC, release after
    down/up BTN...            raw hold / release
    release                   everything neutral
    wait SEC
    shot PATH                 save the guest frame as PNG, wait until written
    run FILE                  steps from another recipe (relative to this one)
# starts a comment.
"""

import os
import sys
import time


class Driver:
    def __init__(self, input_path):
        self.input_path = input_path

    def send(self, line):
        with open(self.input_path, "a", encoding="utf-8") as f:
            f.write(line + "\n")

    def step(self, line, base_dir):
        line = line.split("#", 1)[0].strip()
        words = line.split()
        if not words:
            return
        verb, args = words[0].lower(), words[1:]
        if verb == "press":
            ms = 120
            if args and args[-1].isdigit():
                ms = int(args.pop())
            self.send("down " + " ".join(args))
            time.sleep(ms / 1000)
            self.send("up " + " ".join(args))
            time.sleep(0.25)
        elif verb == "hold":
            sec = float(args.pop())
            self.send("down " + " ".join(args))
            time.sleep(sec)
            self.send("up " + " ".join(args))
        elif verb in ("stick", "trigger"):
            n = 3 if verb == "stick" else 2
            self.send(" ".join([verb] + args[:n]))
            if len(args) > n:
                time.sleep(float(args[n]))
                neutral = ["0", "0"] if verb == "stick" else ["0"]
                self.send(" ".join([verb, args[0]] + neutral))
        elif verb in ("down", "up", "release"):
            self.send(line.strip())
        elif verb == "wait":
            time.sleep(float(args[0]))
        elif verb == "shot":
            path = os.path.abspath(line.split(None, 1)[1].strip())
            if os.path.exists(path):
                os.remove(path)
            self.send("shot " + path)
            deadline = time.time() + 10
            while time.time() < deadline:
                if os.path.exists(path) and os.path.getsize(path) > 0:
                    time.sleep(0.2)
                    print("shot", path)
                    return
                time.sleep(0.1)
            sys.exit(f"shot {path} not written (is the run started with -Drive?)")
        elif verb == "run":
            self.recipe(os.path.join(base_dir, args[0]))
        else:
            sys.exit(f"unknown step: {line}")

    def recipe(self, path):
        with open(path, encoding="utf-8") as f:
            for line in f:
                self.step(line, os.path.dirname(os.path.abspath(path)))


def main():
    if len(sys.argv) < 3:
        sys.exit(__doc__)
    driver = Driver(sys.argv[1])
    for arg in sys.argv[2:]:
        if os.path.isfile(arg):
            driver.recipe(arg)
        else:
            for part in arg.split(";"):
                driver.step(part.strip(), os.getcwd())


if __name__ == "__main__":
    main()
