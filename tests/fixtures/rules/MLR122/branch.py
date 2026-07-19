from manim import *


def choose():
    return True


class UnknownZ(Scene):
    def construct(self):
        low = Square()
        high = Circle()
        if choose():
            high.set_z_index(3)
        self.add(low, high)
        self.bring_to_front(low)
