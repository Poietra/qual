from manim import *


class Good(Scene):
    def construct(self):
        a = Square()
        b = Circle()
        self.add(a)
        # Distinct target: the normal use.
        self.play(Transform(a, b))
        # A copy is a new identity, never the same object.
        target = a.copy()
        self.play(Transform(a, target))
        # A custom path makes even a self-transform visibly move.
        self.play(Transform(a, a, path_arc=PI))
