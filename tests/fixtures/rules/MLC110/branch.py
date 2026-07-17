from manim import *


class Demo(Scene):
    def construct(self):
        outer = VGroup()
        inner = VGroup()
        outer.add(inner)
        if self.flag:
            inner.add(outer)
