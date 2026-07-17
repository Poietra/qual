from manim import *


class Bad(Scene):
    def __init__(self):  # manim-lint: ignore[MLC128]
        self.section = 2

    def construct(self):
        self.wait(1)
