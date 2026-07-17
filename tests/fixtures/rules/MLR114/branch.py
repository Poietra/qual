from manim import *


def make(flag):
    if flag:
        return VMobject()
    return Mobject()


class Branchy(Scene):
    def construct(self):
        # The receiver's class is branch-dependent (and may not be a
        # VMobject at all): silence.
        target = make(True)
        target.set_points_as_corners([[0, 0], [1, 1]])
