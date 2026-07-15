import os
from collections import defaultdict


class Greeter:
    def __init__(self, name):
        self.name = name

    def greet(self):
        print(f"Hello, {self.name}")


if __name__ == "__main__":
    g = Greeter("world")
    g.greet()
