from manim import *


class Demo(Scene):
    def construct(self):
        old = Square()
        new = Circle()
        self.replace(old, new)  # manim-lint: ignore[MLC119]
