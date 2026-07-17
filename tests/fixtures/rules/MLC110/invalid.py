from manim import *


class Demo(Scene):
    def construct(self):
        dot = Dot()
        dot.add(dot)
        outer = VGroup()
        inner = VGroup()
        outer.add(inner)
        inner.add(outer)
