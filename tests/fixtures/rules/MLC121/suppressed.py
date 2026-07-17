from manim import *


class Demo(Scene):
    def construct(self):
        square = Square()
        self.add(square)
        square.add_updater(lambda m: self.wait(1))  # manim-lint: ignore[MLC121]
