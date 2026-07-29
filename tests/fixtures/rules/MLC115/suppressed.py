from manim import *


class Demo(Scene):
    def construct(self):
        square = Square()
        dot = Dot()
        group = VGroup(square, dot)
        self.add(group)
        self.remove(dot)  # qual: ignore[MLC115]
        self.add(group)
