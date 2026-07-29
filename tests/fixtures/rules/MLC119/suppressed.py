from manim import *


class Demo(Scene):
    def construct(self):
        old = Square()
        new = Circle()
        self.replace(old, new)  # qual: ignore[MLC119]
